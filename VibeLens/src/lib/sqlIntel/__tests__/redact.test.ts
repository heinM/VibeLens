import { describe, expect, it } from "vitest";
import { REDACTED, redactDbError, redactForHistory, redactSqlLiterals, stripErrorDetail } from "../redact";

const DUP = 'duplicate key value violates unique constraint "cards_pan_key"\nDETAIL: Key (pan)=(4111-1111-1111-1111) already exists.';

describe("redactDbError", () => {
  it("drops DETAIL and the (col)=(value) group under schemaOnly and none", () => {
    for (const access of ["schemaOnly", "none"] as const) {
      const out = redactDbError(DUP, access);
      expect(out).toBe('duplicate key value violates unique constraint "cards_pan_key"');
      expect(out).not.toContain("4111");
      expect(out).not.toContain("DETAIL");
    }
  });
  it("drops WHERE / CONTEXT / QUERY lines and keeps the SQLSTATE", () => {
    const msg = "SQLSTATE 22P02: invalid input syntax for type integer: \"hunter2\"\nCONTEXT: PL/pgSQL function f(text) line 3\nWHERE: x = 'secret'\nQUERY: select 'secret'";
    const out = redactDbError(msg, "schemaOnly");
    expect(out).toBe(`SQLSTATE 22P02: invalid input syntax for type integer: ${REDACTED}`);
  });
  it("redacts single-quoted literals and value groups inside the primary message", () => {
    expect(redactDbError("value 'abc''def' too long for type character varying(3)", "schemaOnly")).toBe(`value ${REDACTED} too long for type character varying(3)`);
    expect(redactDbError("Key (a, b)=(1, x) is not present in table \"t\"", "schemaOnly")).toBe(`Key ${REDACTED} is not present in table "t"`);
    expect(redactDbError('value "99999999999" is out of range for type integer', "schemaOnly")).toBe(`value ${REDACTED} is out of range for type integer`);
  });
  it("keeps schema object names and the syntax-error token", () => {
    expect(redactDbError('column "ssn" does not exist', "schemaOnly")).toBe('column "ssn" does not exist');
    expect(redactDbError('null value in column "ssn" of relation "people" violates not-null constraint\nDETAIL: Failing row contains (1, Alice, null).', "none")).toBe(
      'null value in column "ssn" of relation "people" violates not-null constraint',
    );
    expect(redactDbError('syntax error at or near "FROM" (position 12)', "schemaOnly")).toBe('syntax error at or near "FROM" (position 12)');
  });
  it("does not leak a value that contains double quotes", () => {
    const out = redactDbError('invalid input syntax for type integer: "a"b-secret-c"', "schemaOnly");
    expect(out).not.toContain("secret");
    expect(out).toBe(`invalid input syntax for type integer: ${REDACTED}`);
  });
  it("does not let a multi-line value smuggle a DETAIL line through", () => {
    const out = redactDbError("invalid input syntax for type json: 'line one\nDETAIL: Key (x)=(leak)\nmore'", "schemaOnly");
    expect(out).not.toContain("leak");
    expect(out).not.toContain("more");
  });
  it("redacts an unterminated quote to the end", () => {
    expect(redactDbError("unterminated quoted string at or near 'abc", "schemaOnly")).not.toContain("abc");
  });
  it("treats unknown access and resultsWithApproval on prod like schemaOnly", () => {
    expect(redactDbError(DUP, null)).not.toContain("4111");
    expect(redactDbError(DUP, "resultsWithApproval", "prod")).not.toContain("4111");
  });
  it("passes the message through when row data may be shared", () => {
    expect(redactDbError(DUP, "results")).toBe(DUP);
    expect(redactDbError(DUP, "resultsWithApproval", "dev")).toBe(DUP);
  });
  it("keeps a HINT line", () => {
    expect(redactDbError('column "nme" does not exist\nHINT: Perhaps you meant to reference the column "t.name".', "schemaOnly")).toContain("HINT:");
  });
});

describe("stripErrorDetail", () => {
  it("removes DETAIL / CONTEXT / WHERE, keeps message and HINT", () => {
    expect(stripErrorDetail(`${DUP}\nHINT: use another\nCONTEXT: SQL statement "insert ..."`)).toBe('duplicate key value violates unique constraint "cards_pan_key"\nHINT: use another');
  });
  it("leaves a single-line message alone", () => {
    expect(stripErrorDetail("relation \"x\" does not exist")).toBe('relation "x" does not exist');
  });
});

describe("redactForHistory", () => {
  it("redacts role and user passwords", () => {
    expect(redactForHistory("CREATE ROLE app LOGIN PASSWORD 'hunter2';")).toBe(`CREATE ROLE app LOGIN PASSWORD '${REDACTED}';`);
    expect(redactForHistory("alter user bob with encrypted password E'it\\'s secret' valid until 'infinity'")).toBe(`alter user bob with encrypted password '${REDACTED}' valid until 'infinity'`);
    expect(redactForHistory("CREATE USER x PASSWORD $$p'w$$")).toBe(`CREATE USER x PASSWORD '${REDACTED}'`);
    expect(redactForHistory("ALTER ROLE r PASSWORD NULL")).toBe("ALTER ROLE r PASSWORD NULL");
  });
  it("redacts password in conninfo strings (dblink, postgres_fdw)", () => {
    expect(redactForHistory("SELECT * FROM dblink('host=db port=5432 user=bob password=hunter2 dbname=x', 'select 1') AS t(a int)")).toBe(
      `SELECT * FROM dblink('host=db port=5432 user=bob password=${REDACTED} dbname=x', 'select 1') AS t(a int)`,
    );
    expect(redactForHistory("SELECT dblink_connect('c', 'host=db password=''my pass'' dbname=x')")).toBe(`SELECT dblink_connect('c', 'host=db password=''${REDACTED}'' dbname=x')`);
    expect(redactForHistory("CREATE USER MAPPING FOR bob SERVER s OPTIONS (user 'bob', password 'p4ss')")).toBe(`CREATE USER MAPPING FOR bob SERVER s OPTIONS (user 'bob', password '${REDACTED}')`);
    expect(redactForHistory("CREATE USER MAPPING FOR bob SERVER s OPTIONS (user 'bob', password = 'p4ss')")).toContain(`'${REDACTED}'`);
    expect(redactForHistory("select dblink_connect('postgresql://bob:s3cret@db/x')")).toBe(`select dblink_connect('postgresql://bob:${REDACTED}@db/x')`);
  });
  it("redacts pgcrypto keys but not the data", () => {
    expect(redactForHistory("INSERT INTO t(v) VALUES (pgp_sym_encrypt('visible data', 'the-key'))")).toBe(`INSERT INTO t(v) VALUES (pgp_sym_encrypt('visible data', '${REDACTED}'))`);
    expect(redactForHistory("select pgp_sym_decrypt(v, 'the-key', 'cipher-algo=aes256') from t")).toBe(`select pgp_sym_decrypt(v, '${REDACTED}', 'cipher-algo=aes256') from t`);
    expect(redactForHistory("select pgp_sym_decrypt_bytea(v, (select 'k' || 'j'))")).toBe(`select pgp_sym_decrypt_bytea(v, (select '${REDACTED}' || '${REDACTED}'))`);
    expect(redactForHistory("select decrypt(v, 'k', 'aes')")).toBe(`select decrypt(v, '${REDACTED}', 'aes')`);
    expect(redactForHistory("select pgp_pub_decrypt(v, dearmor(k), 'pass')")).toBe(`select pgp_pub_decrypt(v, dearmor(k), '${REDACTED}')`);
    expect(redactForHistory("select pgp_sym_encrypt(v, key_col) from t")).toBe("select pgp_sym_encrypt(v, key_col) from t");
    expect(redactForHistory("select crypt('pw', gen_salt('bf'))")).toBe(`select crypt('${REDACTED}', gen_salt('bf'))`);
  });
  it("redacts AWS-style secrets", () => {
    expect(redactForHistory("SET aws.secret_access_key = 'wJalrXUtnFEMI/K7MDENG'")).toBe(`SET aws.secret_access_key = '${REDACTED}'`);
    expect(redactForHistory("COPY t FROM PROGRAM 'AWS_SECRET_ACCESS_KEY=abc123/xyz aws s3 cp s3://b/f -'")).toBe(`COPY t FROM PROGRAM 'AWS_SECRET_ACCESS_KEY=${REDACTED} aws s3 cp s3://b/f -'`);
    expect(redactForHistory("select aws_commons.create_aws_credentials('AKIAABCDEFGHIJKLMNOP', 'sekrit', '')")).toBe(`select aws_commons.create_aws_credentials('${REDACTED}', '${REDACTED}', '${REDACTED}')`);
    expect(redactForHistory("select 'aws_session_token: FQoG-abc' ")).toContain(REDACTED);
    expect(redactForHistory("update cfg set api_key = 'k-123' where id = 1")).toBe(`update cfg set api_key = '${REDACTED}' where id = 1`);
  });
  it("leaves ordinary SQL untouched", () => {
    for (const sql of [
      "select * from users where email = 'a@b.c' and id = 5",
      "select password::text, password_hash from users",
      "update users set password = $1 where id = $2",
      "select 'password' as label",
      "select * from t -- a comment with PASSWORD",
      "",
    ])
      expect(redactForHistory(sql), sql).toBe(sql);
  });
  it("is idempotent", () => {
    const once = redactForHistory("CREATE ROLE a PASSWORD 'x'; select dblink('password=abc')");
    expect(redactForHistory(once)).toBe(once);
  });
});

describe("redactSqlLiterals", () => {
  it("replaces strings, numbers and dollar quotes", () => {
    expect(redactSqlLiterals("select * from t where a = 'x''y' and b > 10.5e3 and c in (1, 2) and d = E'a\\'b' and e = $$z$$ limit 5")).toBe(
      "select * from t where a = ? and b > ? and c in (?, ?) and d = ? and e = ? limit ?",
    );
  });
  it("keeps identifiers, parameters and quoted names", () => {
    expect(redactSqlLiterals('select "col 1", t2.c3, $1 from t2 where x = $2')).toBe('select "col 1", t2.c3, $1 from t2 where x = $2');
  });
  it("blanks comments, which may hold values", () => {
    expect(redactSqlLiterals("select 1 -- for card 4111\n/* secret 42 */")).toBe("select ? -- …\n/* … */");
  });
});
