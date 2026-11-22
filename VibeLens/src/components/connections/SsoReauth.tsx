// "Sign in again" beside an expired-session error: runs the same IAM Identity Center device login
// as the connection form, then lets the caller reconnect or reload.
import { useEffect, useRef, useState } from "react";
import { useQuery, useQueryClient } from "@tanstack/react-query";
import { LogIn } from "lucide-react";
import { api, asError } from "../../ipc/commands";
import type { ConnectionProfile, DeviceLoginStart, ErrorPayload } from "../../ipc/types";
import { useConnect } from "../../hooks/useConnectionStatus";
import { isSsoExpired, ssoLoginsFor } from "../../lib/ssoReauth";
import { Button, ErrorBox } from "../ui";
import { WaitingForBrowser } from "./SsoIdentity";

type Login = { phase: "idle" } | { phase: "starting" } | { phase: "waiting"; start: DeviceLoginStart } | { phase: "failed"; error: ErrorPayload };

/**
 * Renders nothing unless `error` is an expired SSO session and the connection's Identity Center
 * session can be worked out from the profile. `onSignedIn` runs after every session is renewed.
 */
export function SsoReauth({
  profile,
  error,
  onSignedIn,
}: {
  profile: ConnectionProfile;
  error: { message: string } | string | null | undefined;
  onSignedIn: () => void;
}) {
  const qc = useQueryClient();
  const expired = isSsoExpired(error);
  const awsProfiles = useQuery({ queryKey: ["aws-profiles"], queryFn: () => api.awsListProfiles(), enabled: expired, staleTime: 60_000 });
  const sessions = useQuery({ queryKey: ["aws-sso-sessions"], queryFn: () => api.awsSsoSessions(), enabled: expired });
  const [login, setLogin] = useState<Login>({ phase: "idle" });
  const attempt = useRef(0);
  useEffect(
    () => () => {
      attempt.current += 1; // ignore late results after unmount
    },
    [],
  );

  if (!expired) return null;
  const logins = ssoLoginsFor(profile, awsProfiles.data, sessions.data);
  if (logins.length === 0) {
    if (awsProfiles.isPending || sessions.isPending) return null;
    return <p className="text-left text-[12px] text-muted">Edit this connection and sign in to IAM Identity Center again.</p>;
  }

  const signIn = async () => {
    const mine = ++attempt.current;
    setLogin({ phase: "starting" });
    try {
      for (const l of logins) {
        const start = await api.awsSsoStart(l.startUrl, l.region);
        if (mine !== attempt.current) return;
        setLogin({ phase: "waiting", start });
        await api.awsSsoComplete(start.loginId);
        if (mine !== attempt.current) return;
      }
      setLogin({ phase: "idle" });
      void qc.invalidateQueries({ queryKey: ["aws-sso-sessions"] });
      void qc.invalidateQueries({ queryKey: ["aws-sso-accounts"] });
      onSignedIn();
    } catch (e) {
      if (mine !== attempt.current) return;
      setLogin({ phase: "failed", error: asError(e) });
    }
  };
  const cancel = () => {
    attempt.current += 1;
    setLogin({ phase: "idle" });
  };

  if (login.phase === "waiting") {
    return (
      <div className="w-full text-left">
        <WaitingForBrowser start={login.start} onCancel={cancel} />
      </div>
    );
  }
  return (
    <div className="w-full space-y-2 text-left">
      <Button variant="primary" loading={login.phase === "starting"} onClick={() => void signIn()}>
        <LogIn className="size-4" /> Sign in again
      </Button>
      {login.phase === "failed" && <ErrorBox error={login.error} />}
    </div>
  );
}

/** For a navigator that is connected but whose listing failed: sign in, reconnect, load again. */
export function TreeSsoReauth({ profile, error, reload }: { profile: ConnectionProfile; error: { message: string } | null | undefined; reload: () => void }) {
  const connect = useConnect();
  return <SsoReauth profile={profile} error={error} onSignedIn={() => connect.mutate(profile.id, { onSettled: reload })} />;
}
