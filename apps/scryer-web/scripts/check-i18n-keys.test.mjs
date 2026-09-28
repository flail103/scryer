import assert from "node:assert/strict";
import test from "node:test";
import {
  dictionaryKeysInSource,
  translationKeysInSource,
} from "./check-i18n-keys.mjs";

test("collects a key passed on its own", () => {
  assert.deepEqual(translationKeysInSource(`t("status.retry")`), [
    "status.retry",
  ]);
  assert.deepEqual(translationKeysInSource(`translate('status.retry')`), [
    "status.retry",
  ]);
});

test("collects a key passed with interpolation values", () => {
  const source = `const label = t("status.retry", { count: remaining });`;
  assert.deepEqual(translationKeysInSource(source), ["status.retry"]);
});

test("ignores commented-out calls", () => {
  const source = [
    `// t("removed.line.key")`,
    `/* t("removed.block.key") */`,
    `const label = t("label.retry");`,
    `/**`,
    ` * t("removed.doc.key")`,
    ` */`,
  ].join("\n");

  assert.deepEqual(translationKeysInSource(source), ["label.retry"]);
});

test("keeps a call that shares its line with a string containing slashes", () => {
  const source = `const docs = "https://example.test/docs"; t("label.retry");`;
  assert.deepEqual(translationKeysInSource(source), ["label.retry"]);
});

test("collects dictionary keys and skips commented-out entries", () => {
  const source = [
    `export const en = {`,
    `  "label.retry": "Retry",`,
    `  cancel: "Cancel",`,
    `  // "label.removed": "Gone",`,
    `};`,
  ].join("\n");

  assert.deepEqual([...dictionaryKeysInSource(source)].sort(), [
    "cancel",
    "label.retry",
  ]);
});
