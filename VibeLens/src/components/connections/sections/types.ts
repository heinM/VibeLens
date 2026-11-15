import type { ConnectionProfile, IcebergOptions } from "../../../ipc/types";
import type { Draft, S3KeysEdit, SecretEdit } from "../draft";

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
  setPassword: (s: SecretEdit) => void;
  setSshSecret: (s: SecretEdit) => void;
  setIcebergSecret: (s: SecretEdit) => void;
  setS3Keys: (s: S3KeysEdit) => void;
  /** Field-level error, only once the user has tried to save or test. */
  err: (field: string) => string | undefined;
}
