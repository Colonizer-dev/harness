// The deep link into the cockpit's Secrets view: anything that needs a key set — a model provider
// with no key in the model picker, say — calls `openSecrets(id)` and the cockpit opens the page with
// that row in view and its input focused. App provides the function; outside it, it does nothing.
import { createContext, useContext } from "react";

export type OpenSecrets = (id?: string) => void;

/** Null outside App, so a caller can tell there is no Secrets page to send the user to. */
export const SecretsNavContext = createContext<OpenSecrets | null>(null);

export function useOpenSecrets(): OpenSecrets | null {
  return useContext(SecretsNavContext);
}

/** The secret id a model provider's key is saved under, as the Secrets page lists it. */
export function providerSecretId(providerId: string): string {
  return `provider-keys:${providerId}`;
}
