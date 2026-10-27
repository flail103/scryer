import { useCallback, useEffect, useRef, useState } from "react";
import { useClient } from "urql";
import { Ban, CheckCircle2, Clipboard, Edit, KeyRound, Plus, RefreshCw, ShieldAlert } from "lucide-react";
import { AddNewButton } from "@/components/common/add-new-button";
import { ConfirmDialog } from "@/components/common/confirm-dialog";
import { LoadingMark } from "@/components/common/loading-mark";
import { TotpCodeForm } from "@/components/auth/totp-code-form";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Checkbox, CheckboxField } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from "@/components/ui/dialog";
import { IconButton } from "@/components/ui/icon-button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Table,
  TableActionsCell,
  TableActionsHead,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from "@/components/ui/table";
import { IndexerCategoryPicker } from "@/components/views/media-content/indexer-category-picker";
import { useTranslate } from "@/lib/context/translate-context";
import { useUiDateTimeFormat } from "@/lib/context/ui-settings-context";
import { useAuth, type AuthUser } from "@/lib/hooks/use-auth";
import { mfaVerifyStepUpMutation } from "@/lib/graphql/mutations";
import { indexersQuery } from "@/lib/graphql/queries";
import {
  createIndexerClientMutation,
  indexerClientsQuery,
  revokeIndexerClientKeyMutation,
  rotateIndexerClientKeyMutation,
  updateIndexerClientMutation,
  type IndexerClientConfiguration,
  type IndexerClientProtocol,
  type IndexerClientRecord,
} from "@/lib/graphql/indexer-clients";
import type { IndexerRecord } from "@/lib/types";
import { formatUiDateTime } from "@/lib/utils/date-format";
import { selectorId } from "@/lib/utils/dom-ids";
import { indexerClientErrorMessage } from "@/lib/utils/indexer-client-error";
import { indexerClientOutcomeMessage } from "@/lib/utils/indexer-client-outcome";
import { withMfaStepUp } from "@/lib/utils/with-mfa-step-up";

const PROTOCOLS: readonly IndexerClientProtocol[] = ["NEWZNAB", "TORZNAB"];
const PROTOCOL_LABEL_KEYS: Readonly<Record<IndexerClientProtocol, string>> = {
  NEWZNAB: "indexerClients.protocol.newznab",
  TORZNAB: "indexerClients.protocol.torznab",
};

const emptyDraft = (): IndexerClientConfiguration => ({ name: "", enabled: true, protocols: ["NEWZNAB"], grants: [] });
const configurationOf = (client: IndexerClientRecord): IndexerClientConfiguration => ({
  name: client.name,
  enabled: client.enabled,
  protocols: [...client.protocols],
  grants: client.grants.map((grant) => ({
    indexerId: grant.indexerId,
    categories: grant.categories === null ? null : [...grant.categories],
  })),
});

type KeyChange = { client: IndexerClientRecord; operation: "rotate" | "revoke" };
type MutationData = {
  createIndexerClient?: { apiKey: string };
  rotateIndexerClientKey?: { apiKey: string };
};

export function SettingsIndexerClientsContainer() {
  const api = useClient();
  const t = useTranslate();
  const dateTimeFormat = useUiDateTimeFormat();
  const { adoptSession } = useAuth();
  const [clients, setClients] = useState<IndexerClientRecord[] | null>(null);
  const [loading, setLoading] = useState(true);
  const [sources, setSources] = useState<IndexerRecord[]>([]);
  const [draft, setDraft] = useState(emptyDraft);
  const [editorOpen, setEditorOpen] = useState(false);
  const [editing, setEditing] = useState<IndexerClientRecord | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<string | null>(null);
  const [revealedCopied, setRevealedCopied] = useState(false);
  const [confirmation, setConfirmation] = useState<KeyChange | null>(null);
  const [mfaOpen, setMfaOpen] = useState(false);
  const [mfaCode, setMfaCode] = useState("");
  const [mfaBusy, setMfaBusy] = useState(false);
  const [mfaError, setMfaError] = useState<string | null>(null);
  const verification = useRef<((token: string | null) => void) | null>(null);
  const mounted = useRef(false);
  const loadVersion = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; verification.current?.(null); verification.current = null; };
  }, []);

  const load = useCallback(async () => {
    const version = ++loadVersion.current;
    setLoading(true);
    try {
      const result = await api.query<{ indexerClients: IndexerClientRecord[] }>(indexerClientsQuery, {}, { requestPolicy: "network-only" }).toPromise();
      if (!mounted.current || version !== loadVersion.current) return;
      if (result.error || !result.data) { setClients(null); throw result.error; }
      const indexers = await api.query<{ indexers: IndexerRecord[] }>(indexersQuery, {}, { requestPolicy: "network-only" }).toPromise();
      if (!mounted.current || version !== loadVersion.current) return;
      if (indexers.error || !indexers.data) { setClients(null); throw indexers.error; }
      setSources(indexers.data.indexers.filter((source) => !source.supportsManagedChildrenSync));
      setClients(result.data.indexerClients);
    } catch (failure) {
      if (mounted.current && version === loadVersion.current) setError(indexerClientErrorMessage(failure, t));
    } finally {
      if (mounted.current && version === loadVersion.current) setLoading(false);
    }
  }, [api, t]);
  useEffect(() => { void load(); }, [load]);

  const requestMfa = useCallback(() => new Promise<string | null>((resolve) => {
    if (!mounted.current) { resolve(null); return; }
    verification.current = resolve;
    setMfaCode(""); setMfaError(null); setMfaOpen(true);
  }), []);
  const cancelMfa = () => {
    if (mfaBusy) return;
    verification.current?.(null); verification.current = null;
    setMfaOpen(false); setMfaCode("");
  };
  const verifyMfa = async () => {
    if (mfaBusy || !mfaCode.trim()) return;
    setMfaBusy(true); setMfaError(null);
    try {
      const result = await api.mutation<{ mfaVerifyStepUp?: { token: string; user: AuthUser | null; persistSession: boolean } }>(
        mfaVerifyStepUpMutation, { input: { code: mfaCode } }).toPromise();
      if (!mounted.current || !verification.current) return;
      const session = result.data?.mfaVerifyStepUp;
      if (result.error || !session) { setMfaError(t("settings.mfaStepUpFailed")); setMfaCode(""); return; }
      adoptSession(session.token, session.user, session.persistSession);
      verification.current(session.token); verification.current = null;
      setMfaOpen(false); setMfaCode("");
    } catch { if (mounted.current) setMfaError(t("settings.mfaStepUpFailed")); }
    finally { if (mounted.current) setMfaBusy(false); }
  };

  const closeEditor = () => { setEditorOpen(false); setEditing(null); setDraft(emptyDraft()); };

  const mutate = async (document: string, variables: Record<string, unknown>, closeOnSuccess = false) => {
    if (busy) return;
    setBusy(true); setError(null); setRevealed(null);
    try {
      const result = await withMfaStepUp((token) => api.mutation<MutationData>(document, variables,
        token ? { fetchOptions: { headers: { Authorization: `Bearer ${token}` } } } : undefined).toPromise(), requestMfa);
      if (!mounted.current || !result) return;
      if (result.error || !result.data) throw result.error;
      setRevealed(result.data.createIndexerClient?.apiKey ?? result.data.rotateIndexerClientKey?.apiKey ?? null);
      setRevealedCopied(false);
      if (closeOnSuccess) closeEditor();
      await load();
    } catch (failure) { if (mounted.current) setError(indexerClientErrorMessage(failure, t)); }
    finally { if (mounted.current) setBusy(false); }
  };

  const copyRevealed = async () => {
    if (!revealed) return;
    try {
      await navigator.clipboard.writeText(revealed);
      setRevealedCopied(true);
    } catch {
      setError(t("error.fallback.unableToCopyTheAPIKey"));
    }
  };

  const setGrant = (id: string, categories: string[] | null | undefined) => setDraft((current) => ({ ...current,
    grants: [...current.grants.filter((grant) => grant.indexerId !== id),
      ...(categories === undefined ? [] : [{ indexerId: id, categories }])],
  }));
  const startCreate = () => { setEditing(null); setDraft(emptyDraft()); setRevealed(null); setEditorOpen(true); };
  const startEdit = (client: IndexerClientRecord) => {
    setEditing(client); setRevealed(null); setDraft(configurationOf(client)); setEditorOpen(true);
  };
  const toggleEnabled = (client: IndexerClientRecord, enabled: boolean) => void mutate(updateIndexerClientMutation,
    { id: client.id, expectedRevision: client.revision, input: { ...configurationOf(client), enabled } });

  return <div id="settings-indexer-clients" className="flex flex-col gap-4 text-sm">
    <p className="text-[var(--scry-muted3)]">{t("indexerClients.description")}</p>

    {error ? (
      <div
        id="settings-indexer-clients-error"
        role="alert"
        className="flex flex-wrap items-start justify-between gap-3 rounded-[12px] border border-[var(--scry-danger-border)] bg-[var(--scry-danger-bg)] p-3 text-sm text-[var(--scry-danger-text)]"
      >
        <span className="min-w-0 whitespace-pre-wrap break-words">{error}</span>
        <Button type="button" variant="ghost" size="sm" className="h-auto shrink-0 px-2 py-0.5 text-xs" onClick={() => setError(null)}>
          {t("label.dismiss")}
        </Button>
      </div>
    ) : null}

    {revealed ? (
      <div id="settings-indexer-client-revealed" className="space-y-3 rounded-[12px] border border-[var(--scry-warning-border)] bg-[var(--scry-warning-bg)] p-4">
        <div className="flex items-start gap-2">
          <ShieldAlert className="mt-0.5 h-4 w-4 shrink-0 text-[var(--scry-warning-text)]" aria-hidden="true" />
          <p className="font-medium text-[var(--scry-warning-text)]">{t("indexerClients.secretOnce")}</p>
        </div>
        <div className="flex w-fit max-w-full items-center">
          <Input
            id="indexer-client-revealed-key"
            aria-label={t("indexerClients.apiKey")}
            readOnly
            value={revealed}
            size={revealed.length + 2}
            autoComplete="off"
            className="h-10 w-auto max-w-full rounded-r-none font-[var(--font-code)] text-sm text-[var(--scry-ink2)]"
          />
          <IconButton
            id="indexer-client-revealed-copy"
            label={revealedCopied ? t("label.copied") : t("label.copy")}
            tone={revealedCopied ? "enabled" : "neutral"}
            className="-ml-px h-10 shrink-0 rounded-l-none"
            onClick={() => void copyRevealed()}
          >
            {revealedCopied ? <CheckCircle2 className="h-4 w-4" /> : <Clipboard className="h-4 w-4" />}
          </IconButton>
        </div>
        <Button type="button" variant="outline" onClick={() => setRevealed(null)}>{t("label.dismiss")}</Button>
      </div>
    ) : null}

    <Card id="settings-indexer-clients-table-card" className="p-0">
      <CardHeader className="mb-0 flex items-center justify-between gap-3 border-b border-border px-3 py-2">
        <CardTitle className="text-base">{t("indexerClients.existing")}</CardTitle>
        <IconButton
          id="settings-indexer-clients-refresh"
          label={t("label.refresh")}
          disabled={busy || loading}
          onClick={() => { setError(null); void load(); }}
        >
          {loading ? <LoadingMark className="h-4 w-4" /> : <RefreshCw className="h-4 w-4" />}
        </IconButton>
      </CardHeader>
      {loading && clients === null ? (
        <p className="p-4 text-[var(--scry-muted3)]">{t("label.loading")}</p>
      ) : clients === null ? (
        <p className="p-4 text-[var(--scry-muted3)]">{t("status.failedToLoad")}</p>
      ) : clients.length === 0 ? (
        <p id="settings-indexer-clients-empty" className="p-4 text-[var(--scry-muted3)]">{t("indexerClients.none")}</p>
      ) : (
        <Table id="settings-indexer-clients-table" overflow="clip" layout="fixed" density="dense">
          <TableHeader>
            <TableRow>
              <TableHead className="w-[22%]">{t("label.name")}</TableHead>
              <TableHead className="w-[14%]">{t("indexerClients.protocols")}</TableHead>
              <TableHead className="w-[12%] text-center">{t("indexerClients.grants")}</TableHead>
              <TableHead className="w-[8%] text-center">{t("label.enabled")}</TableHead>
              <TableHead className="w-[10%]">{t("indexerClients.key")}</TableHead>
              <TableHead>{t("indexerClients.lastUsed")}</TableHead>
              <TableActionsHead className="w-32">{t("label.actions")}</TableActionsHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {clients.map((client) => (
              <TableRow
                key={client.id}
                id={selectorId("settings-indexer-client-row", client.id)}
                data-ui="settings-table-row"
                data-client-name={client.name}
              >
                <TableCell className="truncate font-medium text-[var(--scry-ink2)]">{client.name}</TableCell>
                <TableCell className="text-[var(--scry-muted3)]">
                  {client.protocols.map((protocol) => t(PROTOCOL_LABEL_KEYS[protocol])).join(" / ")}
                </TableCell>
                <TableCell className="text-center text-[var(--scry-muted3)]">{client.grants.length}</TableCell>
                <TableCell className="text-center">
                  <Checkbox
                    id={selectorId("settings-indexer-client-enabled", client.id)}
                    size="large"
                    checked={client.enabled}
                    disabled={busy}
                    aria-label={`${t("label.enabled")}: ${client.name}`}
                    onCheckedChange={(checked) => toggleEnabled(client, checked === true)}
                  />
                </TableCell>
                <TableCell>
                  <Badge tone={client.keyActive ? "positive" : "negative"}>
                    {t(client.keyActive ? "indexerClients.keyActive" : "indexerClients.keyRevoked")}
                  </Badge>
                </TableCell>
                <TableCell className="text-[var(--scry-muted3)]">
                  <div className="truncate">
                    {client.lastUsedAt
                      ? <time dateTime={client.lastUsedAt}>{formatUiDateTime(client.lastUsedAt, dateTimeFormat)}</time>
                      : t("indexerClients.never")}
                  </div>
                  {client.lastOutcome ? (
                    <div className="truncate text-xs" data-client-outcome={client.lastOutcome}>
                      {indexerClientOutcomeMessage(client.lastOutcome, t)}
                    </div>
                  ) : null}
                </TableCell>
                <TableActionsCell className="w-32">
                  <div className="flex items-center justify-end gap-1">
                    <IconButton
                      id={selectorId("settings-indexer-client-edit", client.id)}
                      label={t("indexerClients.edit")}
                      tone="edit"
                      disabled={busy}
                      onClick={() => startEdit(client)}
                    >
                      <Edit className="h-4 w-4" />
                    </IconButton>
                    <IconButton
                      id={selectorId("settings-indexer-client-rotate", client.id)}
                      label={t("indexerClients.rotate")}
                      tone="upgrade"
                      disabled={busy}
                      onClick={() => setConfirmation({ client, operation: "rotate" })}
                    >
                      <KeyRound className="h-4 w-4" />
                    </IconButton>
                    <IconButton
                      id={selectorId("settings-indexer-client-revoke", client.id)}
                      label={t("indexerClients.revoke")}
                      tone="delete"
                      disabled={busy || !client.keyActive}
                      onClick={() => setConfirmation({ client, operation: "revoke" })}
                    >
                      <Ban className="h-4 w-4" />
                    </IconButton>
                  </div>
                </TableActionsCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </Card>

    {clients !== null && editorOpen ? (
      <Card aria-busy={busy}>
        <form
          id="indexer-client-form"
          onSubmit={(event) => {
            event.preventDefault();
            void mutate(editing ? updateIndexerClientMutation : createIndexerClientMutation,
              editing ? { id: editing.id, expectedRevision: editing.revision, input: draft } : { input: draft }, true);
          }}
        >
          <CardHeader className="flex items-center justify-between gap-3">
            <CardTitle className="text-base">{t(editing ? "indexerClients.edit" : "indexerClients.create")}</CardTitle>
            <label className="flex shrink-0 items-center gap-3">
              <Checkbox
                id="indexer-client-enabled"
                size="large"
                checked={draft.enabled}
                disabled={busy}
                onCheckedChange={(checked) => setDraft({ ...draft, enabled: checked === true })}
              />
              <span className="text-sm font-medium">{t("label.enabled")}</span>
            </label>
          </CardHeader>
          <CardContent className="space-y-4">
            <div className="space-y-1.5">
              <Label htmlFor="indexer-client-name">{t("indexerClients.name")}</Label>
              <Input
                id="indexer-client-name"
                value={draft.name}
                required
                maxLength={120}
                disabled={busy}
                onChange={(event) => setDraft({ ...draft, name: event.target.value })}
              />
            </div>
            <fieldset className="space-y-2">
              <legend className="mb-2 text-sm font-medium">{t("indexerClients.protocols")}</legend>
              <div className="flex flex-wrap gap-6">
                {PROTOCOLS.map((protocol) => (
                  <CheckboxField
                    key={protocol}
                    id={`indexer-client-protocol-${protocol.toLowerCase()}`}
                    label={t(PROTOCOL_LABEL_KEYS[protocol])}
                    checked={draft.protocols.includes(protocol)}
                    disabled={busy}
                    onCheckedChange={(checked) => setDraft({
                      ...draft,
                      protocols: checked === true
                        ? [...draft.protocols, protocol]
                        : draft.protocols.filter((value) => value !== protocol),
                    })}
                  />
                ))}
              </div>
            </fieldset>
            <fieldset className="space-y-3">
              <legend className="mb-1 text-sm font-medium">{t("indexerClients.grants")}</legend>
              <p className="text-[var(--scry-muted3)]">{t("indexerClients.grantsHelp")}</p>
              {sources.map((source) => {
                const grant = draft.grants.find((value) => value.indexerId === source.id);
                return (
                  <div key={source.id} className="space-y-3 rounded-[12px] border border-[var(--scry-line2)] bg-[var(--scry-card2)] p-3">
                    <CheckboxField
                      id={`indexer-client-grant-${source.id}`}
                      label={source.name}
                      checked={!!grant}
                      disabled={busy}
                      onCheckedChange={(checked) => setGrant(source.id, checked === true ? null : undefined)}
                    />
                    {grant ? (
                      <div className="space-y-3 pl-7">
                        <CheckboxField
                          id={`indexer-client-all-categories-${source.id}`}
                          label={t("indexerClients.allCategories")}
                          checked={grant.categories === null}
                          disabled={busy}
                          onCheckedChange={(checked) => setGrant(source.id, checked === true ? null : [])}
                        />
                        {grant.categories !== null ? (
                          <IndexerCategoryPicker
                            scope="RAW"
                            value={grant.categories}
                            disabled={busy}
                            capsCategories={source.capsCategories}
                            triggerId={`client-categories-${source.id}`}
                            panelId={`client-categories-panel-${source.id}`}
                            categoryIdPrefix={`client-category-${source.id}`}
                            categoriesLabel={t("indexerClients.categories")}
                            onChange={(categories) => setGrant(source.id, categories)}
                          />
                        ) : null}
                        {grant.categories?.length === 0 ? (
                          <p className="text-[var(--scry-warning-text)]">{t("indexerClients.noCategories")}</p>
                        ) : null}
                      </div>
                    ) : null}
                  </div>
                );
              })}
            </fieldset>
            <div className="flex flex-wrap gap-2">
              <Button id="indexer-client-save" type="submit" disabled={busy || !draft.name.trim()}>
                {busy ? <LoadingMark /> : null}
                {t(editing ? "indexerClients.save" : "indexerClients.create")}
              </Button>
              <Button type="button" variant="outline" disabled={busy} onClick={closeEditor}>{t("label.cancel")}</Button>
            </div>
          </CardContent>
        </form>
      </Card>
    ) : null}

    {clients !== null && !(editorOpen && editing === null) ? (
      <div className="flex justify-center">
        <AddNewButton
          id="settings-indexer-client-create"
          icon={Plus}
          label={t("indexerClients.create")}
          onClick={startCreate}
          disabled={busy}
        />
      </div>
    ) : null}

    <ConfirmDialog
      open={confirmation !== null}
      title={confirmation
        ? t(confirmation.operation === "rotate" ? "indexerClients.rotateConfirmTitle" : "indexerClients.revokeConfirmTitle",
          { name: confirmation.client.name })
        : ""}
      description={t("indexerClients.keyChangeWarning")}
      confirmLabel={t(confirmation?.operation === "rotate" ? "indexerClients.rotate" : "indexerClients.revoke")}
      cancelLabel={t("label.cancel")}
      isBusy={busy}
      onCancel={() => setConfirmation(null)}
      onConfirm={() => {
        if (!confirmation) return;
        const { client, operation } = confirmation; setConfirmation(null);
        void mutate(operation === "rotate" ? rotateIndexerClientKeyMutation : revokeIndexerClientKeyMutation,
          { id: client.id, expectedRevision: client.revision });
      }}
    />
    <Dialog open={mfaOpen} onOpenChange={(open) => { if (!open) cancelMfa(); }}>
      <DialogContent onInteractOutside={(event) => event.preventDefault()}>
        <DialogHeader>
          <DialogTitle>{t("indexerClients.mfaTitle")}</DialogTitle>
          <DialogDescription>{t("indexerClients.mfaDescription")}</DialogDescription>
        </DialogHeader>
        <TotpCodeForm
          id="indexer-client-mfa-form"
          inputId="indexer-client-mfa-code"
          code={mfaCode}
          title={t("profile.apiKeyMfaCode")}
          description={t("indexerClients.mfaDescription")}
          submitLabel={t("settings.mfaStepUpSubmit")}
          busyLabel={t("settings.mfaStepUpVerifying")}
          cancelLabel={t("label.cancel")}
          busy={mfaBusy}
          allowRecoveryCode
          onCodeChange={setMfaCode}
          onSubmit={verifyMfa}
          onCancel={cancelMfa}
        />
        {mfaError ? <p role="alert" className="text-sm text-destructive">{mfaError}</p> : null}
      </DialogContent>
    </Dialog>
  </div>;
}
