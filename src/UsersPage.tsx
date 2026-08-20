import { FormEvent, useEffect, useState } from "react";
import { useQuery } from "@tanstack/react-query";
import { CircleAlert, KeyRound, Plus, ShieldCheck, Trash2, Users } from "lucide-react";
import { command } from "./api";
import type { AuthStatus, DirectoryGroup, DirectoryUser, PublicModel } from "./types";
import { fetchers, queryKeys } from "./queries";

function optionalLimit(value: string) {
  const trimmed = value.trim();
  if (!trimmed) return null;
  const parsed = Number(trimmed);
  return Number.isFinite(parsed) ? parsed : null;
}

export function LoginPage({ auth, onDone, fail }: { auth: AuthStatus; onDone: () => void; fail: (error: unknown) => void }) {
  const [username, setUsername] = useState("operator");
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const params = new URLSearchParams(window.location.search);
  const oidcError = params.get("oidc_error");

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      await command("login", { username, password });
      onDone();
    } catch (error) {
      fail(error);
    } finally {
      setBusy(false);
    }
  };

  const startOidc = async (provider: string) => {
    try {
      const start = await command<{ authorization_url: string }>("begin_oidc_login", { provider });
      window.location.href = start.authorization_url;
    } catch (error) {
      fail(error);
    }
  };

  return <div className="login-shell">
    <form className="login-card" onSubmit={event => void submit(event)}>
      <div className="brand-mark"><ShieldCheck size={22} /></div>
      <h1>Sign in</h1>
      <p>This gateway is shared on the network. Admin access requires a directory account.</p>
      {auth.tls_fingerprint && <div className="security-note"><ShieldCheck size={17} /><span>TLS fingerprint (SHA-256) <code>{auth.tls_fingerprint}</code></span></div>}
      {oidcError && <div className="toast error" style={{ position: "static" }}><CircleAlert size={17} /><span>{oidcError}</span></div>}
      <label className="field"><span>Username</span><input value={username} onChange={event => setUsername(event.target.value)} autoComplete="username" required /></label>
      <label className="field"><span>Password</span><input type="password" value={password} onChange={event => setPassword(event.target.value)} autoComplete="current-password" required /></label>
      <button className="primary" disabled={busy} type="submit">Sign in</button>
      {auth.oidc_providers.length > 0 && <div className="button-row">{auth.oidc_providers.map(provider => <button key={provider} type="button" className="secondary" onClick={() => void startOidc(provider)}>Continue with {provider}</button>)}</div>}
    </form>
  </div>;
}

export function UsersPage({ publicModels, refresh, success, fail }: { publicModels: PublicModel[]; refresh: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const usersQuery = useQuery({ queryKey: queryKeys.directoryUsers, queryFn: fetchers.directoryUsers });
  const groupsQuery = useQuery({ queryKey: queryKeys.directoryGroups, queryFn: fetchers.directoryGroups });
  const allowQuery = useQuery({ queryKey: queryKeys.oidcAllowlist, queryFn: fetchers.oidcAllowlist });
  const users = usersQuery.data ?? [];
  const groups = groupsQuery.data ?? [];
  const allowlist = allowQuery.data ?? [];
  const [bootstrap, setBootstrap] = useState<string | null>(null);
  const [creating, setCreating] = useState(false);
  const [groupEdit, setGroupEdit] = useState<DirectoryGroup | true | null>(null);
  const [inviteOpen, setInviteOpen] = useState(false);

  useEffect(() => {
    void command<string | null>("reveal_operator_bootstrap").then(setBootstrap).catch(() => undefined);
  }, [users]);

  const reload = async () => {
    await refresh();
    await Promise.all([usersQuery.refetch(), groupsQuery.refetch(), allowQuery.refetch()]);
  };

  return <>
    <div className="page-head"><div><span className="eyebrow">Identity</span><h1>Users</h1><p>Single-tenant directory for this node. Local API keys stay on this machine and are not user passwords.</p></div><div className="button-row"><button className="secondary" onClick={() => setGroupEdit(true)}><Plus size={16} />Group</button><button className="secondary" onClick={() => setInviteOpen(true)}><Plus size={16} />Invite OpenID</button><button className="primary" onClick={() => setCreating(true)}><Plus size={16} />User</button></div></div>
    {bootstrap && <div className="security-note"><KeyRound size={17} /><span>First-run operator password: <code>{bootstrap}</code> Change it after signing in. It is stored only in this node’s secret vault.</span></div>}
    <div className="two-col">
      <div className="panel"><div className="panel-title"><h3>Directory</h3></div>
        {users.map(user => <UserRow key={user.id} user={user} groups={groups} publicModels={publicModels} reload={reload} success={success} fail={fail} />)}
        {!users.length && <p>No users yet.</p>}
      </div>
      <div>
        <div className="panel"><div className="panel-title"><h3>Groups</h3></div>
          {groups.map(group => <div className="model-row" key={group.id}><button type="button" className="grow" style={{ background: "transparent", textAlign: "left" }} onClick={() => setGroupEdit(group)}><strong>{group.name}</strong><span>{group.allowed_model_ids.join(", ") || "no models"} · {group.may_publish ? "publish" : "no publish"} · {group.may_admin ? "admin" : "no admin"}{group.rpm != null ? ` · ${group.rpm} RPM` : ""}</span></button><button className="icon-button danger" onClick={async () => { if (!confirm("Delete this group?")) return; try { await command("delete_directory_group", { id: group.id }); await reload(); success("Group deleted"); } catch (error) { fail(error); } }}><Trash2 size={16} /></button></div>)}
          {!groups.length && <p>Create a group to grant model IDs, publish, or admin.</p>}
        </div>
        <div className="panel" style={{ marginTop: 12 }}><div className="panel-title"><h3>OpenID allowlist</h3></div>
          {allowlist.map(entry => <div className="model-row" key={entry.id}><div className="grow"><strong>{entry.identifier}</strong><span>{entry.provider}{entry.user_id ? " · linked user" : " · creates a user on first sign-in"}</span></div><button className="icon-button danger" onClick={async () => { try { await command("delete_oidc_allowlist", { id: entry.id }); await reload(); success("Invite removed"); } catch (error) { fail(error); } }}><Trash2 size={16} /></button></div>)}
          {!allowlist.length && <p>Unknown GitHub or Google accounts cannot sign in until invited.</p>}
        </div>
      </div>
    </div>
    {creating && <UserModal publicModels={publicModels} groups={groups} close={() => setCreating(false)} reload={reload} success={success} fail={fail} />}
    {groupEdit && <GroupModal group={groupEdit === true ? undefined : groupEdit} close={() => setGroupEdit(null)} publicModels={publicModels} reload={reload} success={success} fail={fail} />}
    {inviteOpen && <InviteModal users={users} close={() => setInviteOpen(false)} reload={reload} success={success} fail={fail} />}
  </>;
}

function UserRow({ user, groups, publicModels, reload, success, fail }: { user: DirectoryUser; groups: DirectoryGroup[]; publicModels: PublicModel[]; reload: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [open, setOpen] = useState(false);
  return <>
    <button className="model-row" style={{ width: "100%", background: "transparent" }} onClick={() => setOpen(true)}>
      <div className="model-icon small"><Users size={17} /></div>
      <div className="grow"><strong>{user.display_name}</strong><span>{user.username} · {user.is_operator ? "operator" : user.group_ids.map(id => groups.find(group => group.id === id)?.name).filter(Boolean).join(", ") || "no group"}</span></div>
      <span className={`badge ${user.disabled_at ? "bad" : user.is_operator ? "good" : "neutral"}`}>{user.disabled_at ? "Disabled" : user.is_operator ? "Operator" : "Local"}</span>
    </button>
    {open && <UserModal user={user} groups={groups} publicModels={publicModels} close={() => setOpen(false)} reload={reload} success={success} fail={fail} />}
  </>;
}

function UserModal({ user, groups, publicModels, close, reload, success, fail }: { user?: DirectoryUser; groups: DirectoryGroup[]; publicModels: PublicModel[]; close: () => void; reload: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [username, setUsername] = useState(user?.username ?? "");
  const [displayName, setDisplayName] = useState(user?.display_name ?? "");
  const [password, setPassword] = useState("");
  const [groupIds, setGroupIds] = useState<string[]>(user?.group_ids ?? []);
  const [overrideModels, setOverrideModels] = useState(user?.allowed_model_ids != null);
  const [models, setModels] = useState((user?.allowed_model_ids ?? []).join(", "));
  const [mayPublish, setMayPublish] = useState<"inherit" | "yes" | "no">(user?.may_publish == null ? "inherit" : user.may_publish ? "yes" : "no");
  const [mayAdmin, setMayAdmin] = useState<"inherit" | "yes" | "no">(user?.may_admin == null ? "inherit" : user.may_admin ? "yes" : "no");
  const [rpm, setRpm] = useState(user?.rpm != null ? String(user.rpm) : "");
  const [tokenBudget, setTokenBudget] = useState(user?.daily_token_budget != null ? String(user.daily_token_budget) : "");
  const [usdBudget, setUsdBudget] = useState(user?.daily_usd_budget != null ? String(user.daily_usd_budget) : "");
  const [busy, setBusy] = useState(false);
  const flag = (value: "inherit" | "yes" | "no") => value === "inherit" ? null : value === "yes";

  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      const allowedModelIds = overrideModels ? models.split(",").map(item => item.trim()).filter(Boolean) : null;
      if (user) {
        await command("update_directory_user", {
          id: user.id,
          input: {
            displayName,
            password: password || null,
            groupIds,
            allowedModelIds: overrideModels ? models.split(",").map(item => item.trim()).filter(Boolean) : [],
            inheritModels: !overrideModels,
            mayPublish: flag(mayPublish) ?? false,
            inheritPublish: mayPublish === "inherit",
            mayAdmin: flag(mayAdmin) ?? false,
            inheritAdmin: mayAdmin === "inherit",
            disabled: !!user.disabled_at,
            rpm: optionalLimit(rpm),
            dailyTokenBudget: optionalLimit(tokenBudget),
            dailyUsdBudget: optionalLimit(usdBudget),
          },
        });
        success("User saved");
      } else {
        await command("create_directory_user", {
          input: {
            username,
            displayName,
            password: password || null,
            groupIds,
            allowedModelIds,
            mayPublish: flag(mayPublish),
            mayAdmin: flag(mayAdmin),
            rpm: optionalLimit(rpm),
            dailyTokenBudget: optionalLimit(tokenBudget),
            dailyUsdBudget: optionalLimit(usdBudget),
          },
        });
        success("User created");
      }
      await reload();
      close();
    } catch (error) {
      fail(error);
    } finally {
      setBusy(false);
    }
  };

  const toggleDisabled = async () => {
    if (!user) return;
    try {
      await command("update_directory_user", { id: user.id, input: { disabled: !user.disabled_at } });
      await reload();
      success(user.disabled_at ? "User enabled" : "User disabled");
      close();
    } catch (error) {
      fail(error);
    }
  };

  return <div className="modal-backdrop" onMouseDown={close}><div className="modal" onMouseDown={event => event.stopPropagation()}>
    <div className="modal-head"><h2>{user ? "Edit user" : "Create user"}</h2><button className="icon-button" onClick={close}>×</button></div>
    <form className="form" onSubmit={event => void submit(event)}>
      <label className="field"><span>Username</span><input value={username} onChange={event => setUsername(event.target.value)} required disabled={!!user} /></label>
      <label className="field"><span>Display name</span><input value={displayName} onChange={event => setDisplayName(event.target.value)} required /></label>
      <label className="field"><span>{user ? "New password (optional)" : "Password (optional until LAN login)"}</span><input type="password" value={password} onChange={event => setPassword(event.target.value)} /></label>
      <div><span className="field-label">Groups</span>{groups.map(group => <label key={group.id} className="capability-editor"><input type="checkbox" checked={groupIds.includes(group.id)} onChange={() => setGroupIds(current => current.includes(group.id) ? current.filter(id => id !== group.id) : [...current, group.id])} />{group.name}</label>)}{!groups.length && <p>No groups yet.</p>}</div>
      <label className="capability-editor"><input type="checkbox" checked={overrideModels} onChange={() => setOverrideModels(!overrideModels)} />Override group model allowlist</label>
      {overrideModels && <label className="field"><span>Allowed public model IDs</span><input value={models} onChange={event => setModels(event.target.value)} placeholder={publicModels.map(model => model.id).slice(0, 3).join(", ")} /></label>}
      <div className="two-fields">
        <label className="field"><span>may_publish</span><select value={mayPublish} onChange={event => setMayPublish(event.target.value as typeof mayPublish)}><option value="inherit">Inherit groups</option><option value="yes">Allow</option><option value="no">Deny</option></select></label>
        <label className="field"><span>may_admin</span><select value={mayAdmin} onChange={event => setMayAdmin(event.target.value as typeof mayAdmin)}><option value="inherit">Inherit groups</option><option value="yes">Allow</option><option value="no">Deny</option></select></label>
      </div>
      <div className="three-fields">
        <label className="field"><span>RPM limit</span><input type="number" min="1" value={rpm} onChange={event => setRpm(event.target.value)} placeholder="unlimited" /></label>
        <label className="field"><span>Daily token budget</span><input type="number" min="1" value={tokenBudget} onChange={event => setTokenBudget(event.target.value)} placeholder="unlimited" /></label>
        <label className="field"><span>Daily USD budget</span><input type="number" min="0" step="any" value={usdBudget} onChange={event => setUsdBudget(event.target.value)} placeholder="unlimited" /></label>
      </div>
      <div className="modal-actions">
        {user && !user.is_operator && <button type="button" className="secondary danger-text" onClick={() => void toggleDisabled()}>{user.disabled_at ? "Enable" : "Disable"}</button>}
        <button type="button" className="secondary" onClick={close}>Cancel</button>
        <button className="primary" disabled={busy}>{busy ? "Saving…" : "Save"}</button>
      </div>
    </form>
  </div></div>;
}

function GroupModal({ group, close, publicModels, reload, success, fail }: { group?: DirectoryGroup; close: () => void; publicModels: PublicModel[]; reload: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [name, setName] = useState(group?.name ?? "");
  const [models, setModels] = useState((group?.allowed_model_ids ?? []).join(", "));
  const [mayPublish, setMayPublish] = useState(group?.may_publish ?? false);
  const [mayAdmin, setMayAdmin] = useState(group?.may_admin ?? false);
  const [rpm, setRpm] = useState(group?.rpm != null ? String(group.rpm) : "");
  const [tokenBudget, setTokenBudget] = useState(group?.daily_token_budget != null ? String(group.daily_token_budget) : "");
  const [usdBudget, setUsdBudget] = useState(group?.daily_usd_budget != null ? String(group.daily_usd_budget) : "");
  const [busy, setBusy] = useState(false);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      await command("save_directory_group", {
        id: group?.id ?? null,
        input: {
          name,
          allowedModelIds: models.split(",").map(item => item.trim()).filter(Boolean),
          mayPublish,
          mayAdmin,
          rpm: optionalLimit(rpm),
          dailyTokenBudget: optionalLimit(tokenBudget),
          dailyUsdBudget: optionalLimit(usdBudget),
        },
      });
      await reload();
      success(group ? "Group saved" : "Group created");
      close();
    } catch (error) {
      fail(error);
    } finally {
      setBusy(false);
    }
  };
  return <div className="modal-backdrop" onMouseDown={close}><div className="modal" onMouseDown={event => event.stopPropagation()}>
    <div className="modal-head"><h2>{group ? "Edit group" : "Create group"}</h2><button className="icon-button" onClick={close}>×</button></div>
    <form className="form" onSubmit={event => void submit(event)}>
      <label className="field"><span>Name</span><input value={name} onChange={event => setName(event.target.value)} required /></label>
      <label className="field"><span>Allowed public model IDs</span><input value={models} onChange={event => setModels(event.target.value)} placeholder={publicModels.map(model => model.id).slice(0, 3).join(", ")} /></label>
      <label className="capability-editor"><input type="checkbox" checked={mayPublish} onChange={() => setMayPublish(!mayPublish)} />may_publish — offer local models to a parent</label>
      <label className="capability-editor"><input type="checkbox" checked={mayAdmin} onChange={() => setMayAdmin(!mayAdmin)} />may_admin</label>
      <div className="three-fields">
        <label className="field"><span>RPM limit</span><input type="number" min="1" value={rpm} onChange={event => setRpm(event.target.value)} placeholder="unlimited" /></label>
        <label className="field"><span>Daily token budget</span><input type="number" min="1" value={tokenBudget} onChange={event => setTokenBudget(event.target.value)} placeholder="unlimited" /></label>
        <label className="field"><span>Daily USD budget</span><input type="number" min="0" step="any" value={usdBudget} onChange={event => setUsdBudget(event.target.value)} placeholder="unlimited" /></label>
      </div>
      <div className="modal-actions"><button type="button" className="secondary" onClick={close}>Cancel</button><button className="primary" disabled={busy}>{busy ? "Saving…" : "Save"}</button></div>
    </form>
  </div></div>;
}

function InviteModal({ users, close, reload, success, fail }: { users: DirectoryUser[]; close: () => void; reload: () => Promise<void>; success: (text: string) => void; fail: (error: unknown) => void }) {
  const [provider, setProvider] = useState("google");
  const [identifier, setIdentifier] = useState("");
  const [userId, setUserId] = useState("");
  const [busy, setBusy] = useState(false);
  const submit = async (event: FormEvent) => {
    event.preventDefault();
    setBusy(true);
    try {
      await command("invite_oidc_identity", { provider, identifier, userId: userId || null });
      await reload();
      success("Identity invited");
      close();
    } catch (error) {
      fail(error);
    } finally {
      setBusy(false);
    }
  };
  return <div className="modal-backdrop" onMouseDown={close}><div className="modal" onMouseDown={event => event.stopPropagation()}>
    <div className="modal-head"><h2>Invite OpenID identity</h2><button className="icon-button" onClick={close}>×</button></div>
    <form className="form" onSubmit={event => void submit(event)}>
      <label className="field"><span>Provider</span><select value={provider} onChange={event => setProvider(event.target.value)}><option value="google">Google</option><option value="github">GitHub</option></select></label>
      <label className="field"><span>Email or GitHub login</span><input value={identifier} onChange={event => setIdentifier(event.target.value)} required placeholder="alice@example.com" /></label>
      <label className="field"><span>Link to existing user (optional)</span><select value={userId} onChange={event => setUserId(event.target.value)}><option value="">Create a user on first sign-in</option>{users.map(user => <option key={user.id} value={user.id}>{user.username}</option>)}</select></label>
      <div className="modal-actions"><button type="button" className="secondary" onClick={close}>Cancel</button><button className="primary" disabled={busy}>Invite</button></div>
    </form>
  </div></div>;
}
