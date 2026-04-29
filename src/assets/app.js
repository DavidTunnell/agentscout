const invoke = window.__TAURI__?.core?.invoke;

async function refreshStatus() {
  const body = document.getElementById("status-body");
  if (!invoke) {
    body.innerHTML = '<p class="muted">Open this page through <code>tauri dev</code>; it needs the Tauri API to display live status.</p>';
    return;
  }
  try {
    const status = await invoke("cmd_get_status");
    body.innerHTML = `
      <div class="status-row"><span class="label">Capture state</span><span>${status.paused ? "Paused" : "Active"}</span></div>
      <div class="status-row"><span class="label">Cadence</span><span>${status.cadence_minutes} min</span></div>
      <div class="status-row"><span class="label">Budget mode</span><span>${status.budget_mode ? "On" : "Off"}</span></div>
      <div class="status-row"><span class="label">Monitors enabled</span><span>${status.monitors_enabled}</span></div>
      <div class="status-row"><span class="label">Schema version</span><span>v${status.schema_version}</span></div>
    `;
    const btn = document.getElementById("toggle-pause");
    if (btn) btn.textContent = status.paused ? "Resume capture" : "Pause capture";
  } catch (e) {
    body.innerHTML = `<p class="muted">Failed to load status: ${e}</p>`;
  }
}

document.getElementById("toggle-pause")?.addEventListener("click", async () => {
  if (!invoke) return;
  await invoke("cmd_toggle_pause");
  refreshStatus();
});

document.getElementById("tick-now")?.addEventListener("click", async () => {
  if (!invoke) return;
  const result = await invoke("cmd_run_tick_now");
  const toast = document.createElement("div");
  toast.textContent = result;
  toast.style.cssText = "position:fixed;bottom:24px;right:24px;background:var(--card);border:1px solid var(--border);padding:12px 16px;border-radius:6px;font-size:13px;";
  document.body.appendChild(toast);
  setTimeout(() => toast.remove(), 3500);
  refreshStatus();
});

// ───────────────────────────────────────────────────────────────────────
// v0.5.5 — Setup-status badge + on-demand run cycle
// ───────────────────────────────────────────────────────────────────────

async function refreshSetupBadge() {
  const body = document.getElementById("setup-body");
  if (!body || !invoke) return;
  try {
    const c = await invoke("cmd_get_credentials_status");
    const items = [];
    items.push(
      c.has_anthropic_key
        ? '<span style="color:#16a34a;">&#10003; Anthropic API key set</span>'
        : '<span style="color:#dc2626;">&#10007; Anthropic API key NOT set — open Settings → paste key → Save before running analysis.</span>'
    );
    items.push(
      c.has_gmail_oauth
        ? '<span style="color:#16a34a;">&#10003; Gmail connected</span>'
        : '<span style="color:#a16207;">&#9888; Gmail not connected (analysis runs locally; no email sent). Wired in v0.5.7.</span>'
    );
    if (c.recipient_email) {
      items.push(`<span class="muted">Recipient: ${c.recipient_email}</span>`);
    }
    body.innerHTML = items.map((i) => `<div>${i}</div>`).join("");
  } catch (e) {
    body.textContent = `Setup status check failed: ${e}`;
  }
}

async function runCycleNow() {
  if (!invoke) return;
  const btn = document.getElementById("run-cycle-btn");
  const result = document.getElementById("run-cycle-result");
  const hoursSel = document.getElementById("cycle-hours");
  const hours = parseInt(hoursSel.value, 10);

  btn.disabled = true;
  result.textContent = "Running cycle (this calls Anthropic; may take 30-90s)...";
  try {
    const r = await invoke("cmd_run_cycle_now", { hoursBack: hours });
    const sentNote = r.email_sent ? " (email sent)" : "";
    result.innerHTML = `
      <span style="color:#16a34a;">&#10003;</span>
      Cycle <code>${r.cycle_id}</code> done — analyzed ${r.n_captures} captures in
      ${r.n_clusters} clusters, produced <strong>${r.n_visible}</strong> visible
      recommendation${r.n_visible === 1 ? "" : "s"}
      (${r.n_suppressed} suppressed) for $${r.estimated_cost_usd.toFixed(4)}${sentNote}.
      <a href="review.html">Open Recommendations →</a>
    `;
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Cycle failed: ${e}</span>`;
  } finally {
    btn.disabled = false;
    refreshSetupBadge();
  }
}

document.getElementById("run-cycle-btn")?.addEventListener("click", runCycleNow);

// ───────────────────────────────────────────────────────────────────────
// v0.5.8 — System health table
// ───────────────────────────────────────────────────────────────────────

function statusBadge(status) {
  const colors = {
    pass: { bg: "#16a34a", text: "PASS" },
    warn: { bg: "#a16207", text: "WARN" },
    fail: { bg: "#dc2626", text: "FAIL" },
  };
  const c = colors[status] || { bg: "#6b7280", text: "?" };
  return `<span style="display:inline-block; min-width: 44px; text-align: center; background: ${c.bg}; color: white; padding: 2px 8px; border-radius: 3px; font-size: 11px; font-weight: 600;">${c.text}</span>`;
}

function escapeHtmlInline(s) {
  return (s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

async function refreshHealth() {
  if (!invoke) return;
  const summary = document.getElementById("health-summary");
  const rowsEl = document.getElementById("health-rows");
  if (!summary || !rowsEl) return;
  try {
    const h = await invoke("cmd_get_system_health");
    summary.innerHTML = `Overall: ${statusBadge(h.overall)} · Active hours: <strong>${(h.active_seconds / 3600).toFixed(2)}h</strong> / ${(h.threshold_seconds / 3600).toFixed(1)}h threshold (cycle <code>${h.cycle_id.slice(0, 8)}…</code>)`;
    rowsEl.innerHTML = h.rows
      .map(
        (r) => `
        <div style="display: flex; gap: 12px; padding: 8px 0; border-bottom: 1px solid var(--border); align-items: flex-start;">
          <div style="min-width: 60px;">${statusBadge(r.status)}</div>
          <div style="flex: 1;">
            <div style="font-weight: 500;">${escapeHtmlInline(r.name)}</div>
            <div class="muted" style="font-size: 12px; margin-top: 2px;">${escapeHtmlInline(r.message)}</div>
          </div>
        </div>
      `
      )
      .join("");
  } catch (e) {
    summary.textContent = `Health check failed: ${e}`;
  }
}

refreshStatus();
refreshSetupBadge();
refreshHealth();
setInterval(refreshStatus, 5000);
setInterval(refreshSetupBadge, 30000);
setInterval(refreshHealth, 15000);
