import type { ConnectionProfile, IcebergOptions, RedshiftOptions } from "../../../ipc/types";
import type { Draft, S3KeysEdit, SecretEdit } from "../draft";
import type { ConfRow } from "../remoteEngineForm";

export interface SectionProps {
  draft: Draft;
  profile: ConnectionProfile;
  /** True when editing a saved connection. */
  editing: boolean;
  /** The saved profile this form started from (null when creating). */
  original: ConnectionProfile | null;
  /** Functional update of the profile. */
  patch: (fn: (p: ConnectionProfile) => ConnectionProfile) => void;
  /** Functional update of `profile.iceberg` (normalised so catalog, storage and engine stay consistent). */
  patchIceberg: (fn: (o: IcebergOptions) => IcebergOptions) => void;
  /** Functional update of `profile.redshift` (normalised so target and sign-in stay consistent). */
  patchRedshift: (fn: (o: RedshiftOptions) => RedshiftOptions) => void;
  setPassword: (s: SecretEdit) => void;
  setSshSecret: (s: SecretEdit) => void;
  setIcebergSecret: (s: SecretEdit) => void;
  setS3Keys: (s: S3KeysEdit) => void;
  /** Trino / Spark password or token. */
  setEngineSecret: (s: SecretEdit) => void;
  /** The Spark session-configuration rows as typed (validated in the draft; see `draftErrors`). */
  setSessionConfRows: (rows: ConfRow[]) => void;
  /** Field-level error, only once the user has tried to save or test. */
  err: (field: string) => string | undefined;
}
