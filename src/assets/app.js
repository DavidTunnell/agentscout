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

refreshStatus();
setInterval(refreshStatus, 5000);
