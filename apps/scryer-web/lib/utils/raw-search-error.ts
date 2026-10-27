export const RAW_SEARCH_REFUSAL_KEYS: Readonly<Record<string, string>> = {
  RAW_SEARCH_RETAINED_STATE_FULL: "indexerSearch.error.capacity",
  RAW_SEARCH_CURSOR_REPEATED: "indexerSearch.error.cursor",
  RAW_SEARCH_OFFSET_INVALID: "indexerSearch.error.offset",
  RAW_SEARCH_CANCELLED: "indexerSearch.error.cancelled",
  RAW_SEARCH_HISTORY_EXPIRED: "indexerSearch.error.expired",
  RAW_SEARCH_ALL_SOURCES_LIMITED: "indexerSearch.error.limited",
  RAW_SEARCH_FAILED: "indexerSearch.error.unavailable",
  RAW_SEARCH_NO_SOURCE_SUCCEEDED: "indexerSearch.error.unavailable",
};
