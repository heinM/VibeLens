import type { Environment } from "../../ipc/types";

// Full class names so Tailwind's scanner picks them up.
export const envBg: Record<Environment, string> = {
  dev: "bg-env-dev",
  test: "bg-env-test",
  staging: "bg-env-staging",
  prod: "bg-env-prod",
};
export const envText: Record<Environment, string> = {
  dev: "text-env-dev",
  test: "text-env-test",
  staging: "text-env-staging",
  prod: "text-env-prod",
};
