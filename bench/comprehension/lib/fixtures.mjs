// Synthetic tool-result documents for the comprehension harness.
//
// Every value here comes from a seeded PRNG and the word lists below, so the
// corpus contains no data from any real system or person and is reproduced
// byte for byte from its seed. The shapes imitate the MCP results tokfold is
// aimed at: a table description, a record query, an inventory listing and an
// audit log, each with a list of objects in which a few optional fields appear
// on only some records (the case tokfold renders as `*{...}` object rows).

/** mulberry32: a small, well-mixed 32-bit PRNG. Same seed, same sequence. */
export function rng(seed) {
  let a = seed >>> 0;
  const next = () => {
    a = (a + 0x6d2b79f5) >>> 0;
    let t = a;
    t = Math.imul(t ^ (t >>> 15), t | 1);
    t ^= t + Math.imul(t ^ (t >>> 7), t | 61);
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
  const int = (lo, hi) => lo + Math.floor(next() * (hi - lo + 1));
  const pick = (xs) => xs[int(0, xs.length - 1)];
  const shuffle = (xs) => {
    const out = xs.slice();
    for (let i = out.length - 1; i > 0; i--) {
      const j = int(0, i);
      [out[i], out[j]] = [out[j], out[i]];
    }
    return out;
  };
  const hex = (n) => {
    let s = "";
    for (let i = 0; i < n; i++) s += "0123456789abcdef"[int(0, 15)];
    return s;
  };
  return { next, int, pick, shuffle, hex };
}

const NOUNS = [
  "asset", "badge", "budget", "cable", "catalog", "channel", "contract", "cost",
  "device", "district", "export", "facility", "gateway", "invoice", "ledger",
  "license", "meter", "model", "network", "order", "outlet", "panel", "permit",
  "pipeline", "plan", "policy", "portal", "printer", "project", "queue", "rack",
  "region", "relay", "report", "request", "route", "schedule", "sensor", "server",
  "service", "shipment", "site", "socket", "spare", "stage", "storage", "supply",
  "switch", "ticket", "token", "trigger", "vendor", "vehicle", "volume", "window",
  "workflow", "zone",
];
const ADJECTIVES = [
  "intermittent", "slow", "missing", "duplicate", "stale", "failed", "partial",
  "delayed", "locked", "expired", "corrupt", "unreachable", "noisy", "blocked",
];
const VERBS = [
  "after restart", "during backup", "on login", "since upgrade", "at night",
  "under load", "for new users", "in the east wing", "after failover",
  "on export", "when syncing", "after maintenance",
];

const title = (s) => s.charAt(0).toUpperCase() + s.slice(1);

function describeTable(r) {
  const n = r.int(10, 60);
  const table = `u_${r.pick(NOUNS)}_${r.pick(NOUNS)}`;
  const types = ["string", "string", "string", "integer", "boolean", "reference", "glide_date_time", "choice"];
  const names = new Set();
  const order = r.shuffle(Array.from({ length: n }, (_, i) => (i + 1) * 10));
  const fields = [];
  for (let i = 0; i < n; i++) {
    let name;
    do name = `u_${r.pick(NOUNS)}${r.int(0, 3) === 0 ? "_" + r.pick(NOUNS) : ""}`;
    while (names.has(name));
    names.add(name);
    const type = r.pick(types);
    const f = {
      name,
      label: title(name.slice(2).replace(/_/g, " ")),
      type,
      max_length: type === "string" ? r.pick([40, 100, 255]) : type === "integer" ? 40 : 32,
      mandatory: r.int(0, 3) === 0,
      read_only: r.int(0, 5) === 0,
      display_order: order[i],
    };
    fields.push(f);
  }
  // Optional fields on only a few records.
  const refs = r.shuffle(fields.filter((f) => f.type === "reference")).slice(0, 3);
  for (const f of refs) f.reference = r.pick(["sys_user", "cmn_location", "core_company", "cmdb_ci"]);
  for (const f of r.shuffle(fields).slice(0, r.int(1, 4))) f.default_value = r.pick(["true", "0", "draft", "javascript:gs.now()"]);
  for (const f of r.shuffle(fields).slice(0, r.int(1, 3))) f.dependent_on = r.pick(fields).name;
  return {
    tool: "describe_table",
    pretty: true,
    doc: { table, label: title(table.slice(2).replace(/_/g, " ")), super_class: r.pick(["task", null]), fields },
  };
}

const STATES = ["New", "In Progress", "On Hold", "Resolved", "Closed"];
const PRIORITIES = ["1 - Critical", "2 - High", "3 - Moderate", "4 - Low"];
const GROUPS = ["Service Desk", "Network", "Database", "Field Services", "Hardware"];

function queryTable(r) {
  const n = r.int(8, 80);
  const records = [];
  const base = r.int(10000, 90000);
  const numbers = r.shuffle(Array.from({ length: n }, (_, i) => base + i * r.int(1, 3) + i));
  for (let i = 0; i < n; i++) {
    const rec = {
      sys_id: r.hex(32),
      number: `INC00${numbers[i]}`,
      short_description: `${title(r.pick(ADJECTIVES))} ${r.pick(NOUNS)} ${r.pick(VERBS)}`,
      state: r.pick(STATES),
      priority: r.pick(PRIORITIES),
      category: r.pick(["hardware", "software", "network", "inquiry"]),
      assignment_group: r.pick(GROUPS),
      reopen_count: r.pick([0, 0, 0, 1, 1, 2, 3]),
      business_duration_min: r.int(5, 9000),
      opened_at: `2026-${String(r.int(1, 10)).padStart(2, "0")}-${String(r.int(1, 28)).padStart(2, "0")} ${String(r.int(0, 23)).padStart(2, "0")}:${String(r.int(0, 59)).padStart(2, "0")}:00`,
    };
    records.push(rec);
  }
  for (const rec of r.shuffle(records).slice(0, r.int(1, 4))) rec.close_code = r.pick(["Solved (Permanently)", "Solved (Work Around)", "Not Solved"]);
  for (const rec of r.shuffle(records).slice(0, r.int(1, 3))) rec.parent_incident = `INC00${r.int(10000, 99999)}`;
  return { tool: "query_table", pretty: false, doc: { table: "incident", records } };
}

function inventory(r) {
  const n = r.int(8, 70);
  const items = [];
  const ids = new Set();
  for (let i = 0; i < n; i++) {
    let id;
    do id = `vm-${r.hex(6)}`;
    while (ids.has(id));
    ids.add(id);
    items.push({
      id,
      name: `${r.pick(NOUNS)}-${r.pick(NOUNS)}-${String(r.int(1, 99)).padStart(2, "0")}`,
      status: r.pick(["running", "running", "stopped", "pending"]),
      region: r.pick(["eu-west", "us-east", "ap-south"]),
      cpu: r.pick([2, 4, 8, 16]),
      memory_mb: r.pick([2048, 4096, 8192, 16384, 32768]),
      uptime_hours: r.int(0, 20000),
      monitored: r.int(0, 4) !== 0,
    });
  }
  for (const it of r.shuffle(items).slice(0, r.int(1, 4))) it.last_error = r.pick(["disk quota exceeded", "health check timeout", "kernel panic"]);
  for (const it of r.shuffle(items).slice(0, r.int(1, 2))) it.maintenance_window = r.pick(["sun 02:00-04:00", "sat 22:00-23:00"]);
  return { tool: "list_instances", pretty: true, doc: { items, next_cursor: r.hex(16) } };
}

function auditLog(r) {
  const n = r.int(8, 60);
  const events = [];
  for (let i = 0; i < n; i++) {
    const status = r.pick([200, 200, 200, 201, 204, 403, 404, 500]);
    events.push({
      event_id: `evt_${r.hex(10)}`,
      actor: `svc-${r.pick(NOUNS)}`,
      action: r.pick(["create", "update", "delete", "login", "export"]),
      resource: `${r.pick(NOUNS)}/${r.int(100, 999)}`,
      status_code: status,
      latency_ms: r.int(3, 4000),
      ts: `2026-11-${String(r.int(1, 14)).padStart(2, "0")}T${String(r.int(0, 23)).padStart(2, "0")}:${String(r.int(0, 59)).padStart(2, "0")}:${String(r.int(0, 59)).padStart(2, "0")}Z`,
    });
  }
  for (const e of r.shuffle(events).slice(0, r.int(1, 4))) e.error_message = r.pick(["permission denied", "not found", "upstream timeout"]);
  for (const e of r.shuffle(events).slice(0, r.int(1, 2))) e.retry_of = `evt_${r.hex(10)}`;
  return { tool: "audit_events", pretty: false, doc: { result: { page: 1, events } } };
}

const FAMILIES = [describeTable, queryTable, inventory, auditLog];

/**
 * Generate `count` documents from `seed`. Returns `{ id, tool, text }` with the
 * exact bytes a tool would have returned (pretty-printed with two spaces for
 * the families marked so, compact otherwise) and a trailing newline.
 */
export function generateDocuments(seed, count) {
  const r = rng(seed);
  const out = [];
  for (let i = 0; i < count; i++) {
    const { tool, pretty, doc } = FAMILIES[i % FAMILIES.length](r);
    const text = JSON.stringify(doc, null, pretty ? 2 : undefined) + "\n";
    out.push({ id: `D${String(i + 1).padStart(2, "0")}`, tool, text });
  }
  return out;
}
