import { useCallback, useEffect, useRef, useState } from "react";
import { useClient } from "urql";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Checkbox } from "@/components/ui/checkbox";
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogDescription } from "@/components/ui/dialog";
import { ConfirmDialog } from "@/components/common/confirm-dialog";
import { TotpCodeForm } from "@/components/auth/totp-code-form";
import { IndexerCategoryPicker } from "@/components/views/media-content/indexer-category-picker";
import { useTranslate } from "@/lib/context/translate-context";
import { useUiDateTimeFormat } from "@/lib/context/ui-settings-context";
import { formatUiDateTime } from "@/lib/utils/date-format";
import { useAuth, type AuthUser } from "@/lib/hooks/use-auth";
import { indexersQuery } from "@/lib/graphql/queries";
import { mfaVerifyStepUpMutation } from "@/lib/graphql/mutations";
import { createIndexerClientMutation, updateIndexerClientMutation, rotateIndexerClientKeyMutation,
  revokeIndexerClientKeyMutation, indexerClientsQuery,
  type IndexerClientConfiguration, type IndexerClientRecord, type IndexerClientProtocol,
} from "@/lib/graphql/indexer-clients";
import { indexerClientErrorMessage } from "@/lib/utils/indexer-client-error";
import { indexerClientOutcomeMessage } from "@/lib/utils/indexer-client-outcome";
import { withMfaStepUp } from "@/lib/utils/with-mfa-step-up";
import type { IndexerRecord } from "@/lib/types";

const emptyDraft = (): IndexerClientConfiguration => ({ name: "", enabled: true, protocols: ["NEWZNAB"], grants: [] });
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
  const [sources, setSources] = useState<IndexerRecord[]>([]);
  const [draft, setDraft] = useState(emptyDraft);
  const [editing, setEditing] = useState<IndexerClientRecord | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [revealed, setRevealed] = useState<string | null>(null);
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

  const mutate = async (document: string, variables: Record<string, unknown>, resetDraft = false) => {
    if (busy) return;
    setBusy(true); setError(null); setRevealed(null);
    try {
      const result = await withMfaStepUp((token) => api.mutation<MutationData>(document, variables,
        token ? { fetchOptions: { headers: { Authorization: `Bearer ${token}` } } } : undefined).toPromise(), requestMfa);
      if (!mounted.current || !result) return;
      if (result.error || !result.data) throw result.error;
      setRevealed(result.data.createIndexerClient?.apiKey ?? result.data.rotateIndexerClientKey?.apiKey ?? null);
      if (resetDraft) { setDraft(emptyDraft()); setEditing(null); }
      await load();
    } catch (failure) { if (mounted.current) setError(indexerClientErrorMessage(failure, t)); }
    finally { if (mounted.current) setBusy(false); }
  };

  const setGrant = (id: string, categories: string[] | null | undefined) => setDraft((current) => ({ ...current,
    grants: [...current.grants.filter((grant) => grant.indexerId !== id),
      ...(categories === undefined ? [] : [{ indexerId: id, categories }])],
  }));
  const edit = (client: IndexerClientRecord) => {
    setEditing(client); setRevealed(null);
    setDraft({ name: client.name, enabled: client.enabled, protocols: [...client.protocols],
      grants: client.grants.map((grant) => ({ ...grant, categories: grant.categories === null ? null : [...grant.categories] })) });
  };

  return <section className="space-y-6" id="settings-indexer-clients">
    <p className="text-sm text-[var(--scry-muted3)]">{t("indexerClients.description")}</p>
    {error ? <p role="alert" className="text-sm text-destructive">{error}</p> : null}
    <Button variant="outline" disabled={busy} onClick={() => { setError(null); void load(); }}>{t("indexerClients.refresh")}</Button>
    {revealed ? <div className="space-y-3 rounded-xl border p-4">
      <p>{t("indexerClients.secretOnce")}</p>
      <Input id="indexer-client-revealed-key" aria-label={t("indexerClients.apiKey")} readOnly value={revealed} autoComplete="off" />
      <Button variant="outline" onClick={() => setRevealed(null)}>{t("indexerClients.dismiss")}</Button>
    </div> : null}
    {clients !== null ? <>
      <div className="space-y-3">{clients.map((client) => <article key={client.id} id={`indexer-client-${client.id}`} data-client-name={client.name} className="flex flex-wrap items-center justify-between gap-4 rounded-xl border p-4">
        <div><h3 className="font-medium">{client.name}</h3>
          <p className="text-sm text-[var(--scry-muted3)]">{t(client.enabled ? "indexerClients.enabled" : "indexerClients.disabled")} · {t(client.keyActive ? "indexerClients.keyActive" : "indexerClients.keyRevoked")}</p>
          <p className="text-sm">{client.protocols.join(" / ")} · {t("indexerClients.sourceCount", { count: client.grants.length })}</p>
          <p className="text-sm">{t("indexerClients.lastUsed")}: {client.lastUsedAt
            ? <time dateTime={client.lastUsedAt}>{formatUiDateTime(client.lastUsedAt, dateTimeFormat)}</time>
            : t("indexerClients.never")}</p>
          {client.lastOutcomeCode ? <p className="text-sm" data-client-outcome={client.lastOutcomeCode}>
            {indexerClientOutcomeMessage(client.lastOutcomeCode, t)}
          </p> : null}
        </div>
        <div className="flex flex-wrap gap-2">
          <Button variant="outline" disabled={busy} onClick={() => edit(client)}>{t("indexerClients.edit")}</Button>
          <Button variant="outline" disabled={busy} onClick={() => setConfirmation({ client, operation: "rotate" })}>{t("indexerClients.rotate")}</Button>
          <Button variant="destructive" disabled={busy || !client.keyActive} onClick={() => setConfirmation({ client, operation: "revoke" })}>{t("indexerClients.revoke")}</Button>
        </div>
      </article>)}</div>
      <form id="indexer-client-form" className="space-y-4 rounded-xl border p-4" onSubmit={(event) => {
        event.preventDefault();
        void mutate(editing ? updateIndexerClientMutation : createIndexerClientMutation,
          editing ? { id: editing.id, expectedRevision: editing.revision, input: draft } : { input: draft }, true);
      }}>
        <h3 className="font-medium">{t(editing ? "indexerClients.edit" : "indexerClients.create")}</h3>
        <Label htmlFor="indexer-client-name">{t("indexerClients.name")}</Label>
        <Input id="indexer-client-name" value={draft.name} required maxLength={120} disabled={busy}
          onChange={(event) => setDraft({ ...draft, name: event.target.value })} />
        <label className="flex items-center gap-2"><Checkbox id="indexer-client-enabled" checked={draft.enabled} disabled={busy}
          onCheckedChange={(checked) => setDraft({ ...draft, enabled: checked === true })} />{t("indexerClients.enabled")}</label>
        <fieldset className="flex gap-4"><legend>{t("indexerClients.protocols")}</legend>
          {(["NEWZNAB", "TORZNAB"] as IndexerClientProtocol[]).map((protocol) => <label key={protocol} className="flex items-center gap-2">
            <Checkbox id={`indexer-client-protocol-${protocol.toLowerCase()}`} checked={draft.protocols.includes(protocol)} disabled={busy} onCheckedChange={(checked) => setDraft({ ...draft,
              protocols: checked === true ? [...draft.protocols, protocol] : draft.protocols.filter((value) => value !== protocol) })} />
            {protocol === "NEWZNAB" ? "Newznab" : "Torznab"}
          </label>)}
        </fieldset>
        <fieldset className="space-y-3"><legend>{t("indexerClients.grants")}</legend>
          <p className="text-sm text-[var(--scry-muted3)]">{t("indexerClients.grantsHelp")}</p>
          {sources.map((source) => {
            const grant = draft.grants.find((value) => value.indexerId === source.id);
            return <div key={source.id} className="space-y-2 rounded-lg border p-3">
              <label className="flex items-center gap-2"><Checkbox id={`indexer-client-grant-${source.id}`} checked={!!grant} disabled={busy}
                onCheckedChange={(checked) => setGrant(source.id, checked === true ? null : undefined)} />{source.name}</label>
              {grant ? <>
                <label className="flex items-center gap-2 text-sm"><Checkbox id={`indexer-client-all-categories-${source.id}`} checked={grant.categories === null} disabled={busy}
                  onCheckedChange={(checked) => setGrant(source.id, checked === true ? null : [])} />{t("indexerClients.allCategories")}</label>
                {grant.categories !== null ? <IndexerCategoryPicker scope="RAW" value={grant.categories} disabled={busy}
                  capsCategories={source.capsCategories} triggerId={`client-categories-${source.id}`}
                  panelId={`client-categories-panel-${source.id}`} categoryIdPrefix={`client-category-${source.id}`}
                  categoriesLabel={t("indexerClients.categories")} onChange={(categories) => setGrant(source.id, categories)} /> : null}
                {grant.categories?.length === 0 ? <p className="text-sm">{t("indexerClients.noCategories")}</p> : null}
              </> : null}
            </div>;
          })}
        </fieldset>
        <div className="flex gap-2"><Button type="submit" disabled={busy || !draft.name.trim()}>{t(editing ? "indexerClients.save" : "indexerClients.create")}</Button>
          {editing ? <Button type="button" variant="outline" disabled={busy} onClick={() => { setEditing(null); setDraft(emptyDraft()); }}>{t("label.cancel")}</Button> : null}
        </div>
      </form>
    </> : null}
    <ConfirmDialog open={confirmation !== null} title={t(confirmation?.operation === "rotate" ? "indexerClients.rotate" : "indexerClients.revoke")}
      description={t("indexerClients.keyChangeWarning")} confirmLabel={t("indexerClients.confirm")} cancelLabel={t("label.cancel")}
      isBusy={busy} onCancel={() => setConfirmation(null)} onConfirm={() => {
        if (!confirmation) return;
        const { client, operation } = confirmation; setConfirmation(null);
        void mutate(operation === "rotate" ? rotateIndexerClientKeyMutation : revokeIndexerClientKeyMutation,
          { id: client.id, expectedRevision: client.revision });
      }} />
    <Dialog open={mfaOpen} onOpenChange={(open) => { if (!open) cancelMfa(); }}><DialogContent onInteractOutside={(event) => event.preventDefault()}>
      <DialogHeader><DialogTitle>{t("profile.apiKeyMfaTitle")}</DialogTitle><DialogDescription>{t("profile.apiKeyMfaDescription")}</DialogDescription></DialogHeader>
      <TotpCodeForm id="indexer-client-mfa-form" inputId="indexer-client-mfa-code" code={mfaCode} title={t("profile.apiKeyMfaCode")}
        description={t("profile.apiKeyMfaDescription")} submitLabel={t("settings.mfaStepUpSubmit")} busyLabel={t("settings.mfaStepUpVerifying")}
        cancelLabel={t("label.cancel")} busy={mfaBusy} allowRecoveryCode onCodeChange={setMfaCode} onSubmit={verifyMfa} onCancel={cancelMfa} />
      {mfaError ? <p role="alert" className="text-sm text-destructive">{mfaError}</p> : null}
    </DialogContent></Dialog>
  </section>;
}
