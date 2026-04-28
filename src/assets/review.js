const invoke = window.__TAURI__?.core?.invoke;

const cycleBody = document.getElementById("cycle-body");
const recList = document.getElementById("rec-list");
const showSuppressed = document.getElementById("show-suppressed");
const refreshBtn = document.getElementById("refresh-btn");

function escapeHtml(s) {
  return (s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

function tierLabel(id) {
  return ({
    "time-reclaimers": "Time Reclaimers",
    "expertise-amplifiers": "Expertise Amplifiers",
    "capability-unlocks": "Capability Unlocks",
  })[id] || id;
}

async function refreshCycle() {
  if (!invoke) {
    cycleBody.innerHTML = '<p class="muted">Open through <code>tauri dev</code>.</p>';
    return;
  }
  try {
    const status = await invoke("cmd_get_cycle_status");
    cycleBody.innerHTML = `
      <div class="status-row"><span class="label">Cycle</span><span><code>${status.cycle_id.slice(0,8)}…</code></span></div>
      <div class="status-row"><span class="label">Active hours</span><span>${status.active_hours.toFixed(1)} / ${status.threshold_hours}</span></div>
      <div class="status-row"><span class="label">Progress</span><span>${status.progress_pct.toFixed(0)}%</span></div>
      <div class="status-row"><span class="label">Disposition server</span><span><code>${status.disposition_server_origin}</code></span></div>
    `;
  } catch (e) {
    cycleBody.innerHTML = `<p class="muted">Status unavailable: ${e}</p>`;
  }
}

async function refreshRecs() {
  if (!invoke) return;
  recList.innerHTML = '<p class="muted">Loading...</p>';
  try {
    const recs = await invoke("cmd_list_recommendations", {
      includeSuppressed: showSuppressed.checked,
    });
    if (recs.length === 0) {
      recList.innerHTML = '<p class="muted">No recommendations yet. Recommendations are produced when an analysis cycle completes.</p>';
      return;
    }
    recList.innerHTML = recs.map(renderRec).join("");
    recList.querySelectorAll("button[data-disposition]").forEach((btn) => {
      btn.addEventListener("click", () => dispose(btn.dataset.recId, btn.dataset.disposition));
    });
  } catch (e) {
    recList.innerHTML = `<p class="muted">Failed to load: ${e}</p>`;
  }
}

function renderRec(r) {
  const status = r.disposition
    ? `<span class="rec-disposed">${escapeHtml(r.disposition)}</span>`
    : "";
  const supTag = r.suppressed ? '<span class="rec-suppressed">Suppressed (below threshold)</span>' : "";
  const conf = r.confidence != null ? `${Math.round(r.confidence * 100)}%` : "?";
  const freq = r.frequency_per_week != null ? `${r.frequency_per_week.toFixed(1)}/wk` : null;
  const time = r.est_time_saved_minutes != null && r.frequency_per_week != null
    ? `~${Math.round(r.est_time_saved_minutes * r.frequency_per_week)} min/wk saved`
    : null;
  const sv = r.strategic_value ? escapeHtml(r.strategic_value) : null;

  const meta = [freq, time, sv, `Build ${escapeHtml(r.build_complexity || "?")}`, `Conf ${conf}`]
    .filter(Boolean).join(" · ");

  return `
    <div class="rec-card${r.suppressed ? " rec-suppressed-card" : ""}">
      <div class="rec-tier">${escapeHtml(tierLabel(r.tier_id))}</div>
      <div class="rec-head">
        <h3>${escapeHtml(r.name)}</h3>
        ${status}${supTag}
      </div>
      <p class="rec-desc">${escapeHtml(r.description || "")}</p>
      <p class="rec-pattern muted"><strong>Observed:</strong> ${escapeHtml(r.observed_pattern || "")}</p>
      <p class="muted">${meta}</p>
      <div class="rec-actions">
        <button data-rec-id="${r.id}" data-disposition="implemented">Implemented</button>
        <button data-rec-id="${r.id}" data-disposition="not_interested" class="secondary">Not Interested</button>
        <button data-rec-id="${r.id}" data-disposition="maybe_later" class="secondary">Maybe Later</button>
      </div>
    </div>
  `;
}

async function dispose(recId, action) {
  if (!invoke) return;
  try {
    await invoke("cmd_set_disposition", { recId, action });
    refreshRecs();
  } catch (e) {
    alert(`Failed: ${e}`);
  }
}

showSuppressed.addEventListener("change", refreshRecs);
refreshBtn.addEventListener("click", () => { refreshCycle(); refreshRecs(); });

refreshCycle();
refreshRecs();
