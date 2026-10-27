export type IndexerClientProtocol = "NEWZNAB" | "TORZNAB";
export type IndexerClientGrant = { indexerId: string; categories: string[] | null };
export type IndexerClientConfiguration = {
  name: string;
  enabled: boolean;
  protocols: IndexerClientProtocol[];
  grants: IndexerClientGrant[];
};
export type IndexerClientRecord = IndexerClientConfiguration & {
  id: string;
  revision: string;
  keyActive: boolean;
  lastUsedAt: string | null;
  lastOutcomeCode: string | null;
};

const clientFields = `id name enabled protocols grants { indexerId categories }
  revision keyActive lastUsedAt lastOutcomeCode`;
export const indexerClientsQuery = `query IndexerClients { indexerClients { ${clientFields} } }`;
export const createIndexerClientMutation = `mutation CreateIndexerClient($input: IndexerClientConfigurationInput!) {
  createIndexerClient(input: $input) { apiKey client { ${clientFields} } }
}`;
export const updateIndexerClientMutation = `mutation UpdateIndexerClient($id: ID!, $expectedRevision: String!, $input: IndexerClientConfigurationInput!) {
  updateIndexerClient(id: $id, expectedRevision: $expectedRevision, input: $input) { ${clientFields} }
}`;
export const rotateIndexerClientKeyMutation = `mutation RotateIndexerClientKey($id: ID!, $expectedRevision: String!) {
  rotateIndexerClientKey(id: $id, expectedRevision: $expectedRevision) { apiKey client { ${clientFields} } }
}`;
export const revokeIndexerClientKeyMutation = `mutation RevokeIndexerClientKey($id: ID!, $expectedRevision: String!) {
  revokeIndexerClientKey(id: $id, expectedRevision: $expectedRevision) { ${clientFields} }
}`;
