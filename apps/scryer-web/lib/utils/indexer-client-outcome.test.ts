import assert from "node:assert/strict";
import test from "node:test";
import { indexerClientOutcomeMessage } from "./indexer-client-outcome.ts";

test("client outcomes translate typed codes without exposing unknown server text", () => {
  const t = (key: string) => `translated:${key}`;
  assert.equal(indexerClientOutcomeMessage("INDEXER_CLIENTS_SEARCH_PARTIAL", t), "translated:indexerClients.outcome.partial");
  assert.equal(indexerClientOutcomeMessage("INDEXER_CLIENTS_REQUEST_UNAVAILABLE", t), "translated:indexerClients.outcome.unavailable");
  assert.equal(indexerClientOutcomeMessage("arbitrary legacy sentence", t), "translated:indexerClients.outcome.unknown");
  assert.equal(indexerClientOutcomeMessage("toString", t), "translated:indexerClients.outcome.unknown");
});
