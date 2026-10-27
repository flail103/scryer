export const INDEXER_CLIENT_OUTCOME_KEYS: Readonly<Record<string, string>> = {
  INDEXER_CLIENTS_CAPS_READY: "indexerClients.outcome.caps",
  INDEXER_CLIENTS_SEARCH_READY: "indexerClients.outcome.search",
  INDEXER_CLIENTS_SEARCH_PARTIAL: "indexerClients.outcome.partial",
  INDEXER_CLIENTS_ARTIFACT_READY: "indexerClients.outcome.artifact",
  INDEXER_CLIENTS_REQUEST_REFUSED: "indexerClients.outcome.refused",
  INDEXER_CLIENTS_REQUEST_UNAVAILABLE: "indexerClients.outcome.unavailable",
  INDEXER_CLIENTS_REQUEST_FAILED: "indexerClients.outcome.failed",
};

export function indexerClientOutcomeMessage(code: string, t: (key: string) => string): string {
  return t(Object.hasOwn(INDEXER_CLIENT_OUTCOME_KEYS, code)
    ? INDEXER_CLIENT_OUTCOME_KEYS[code]! : "indexerClients.outcome.unknown");
}
