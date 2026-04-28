const invoke = window.__TAURI__?.core?.invoke;

const paths = {
  windows: "%APPDATA%\\AgentScout\\",
  macos: "~/Library/Application Support/AgentScout/",
  linux: "~/.local/share/agentscout/",
};

const platform = (() => {
  const ua = navigator.userAgent;
  if (ua.includes("Windows")) return "windows";
  if (ua.includes("Mac")) return "macos";
  return "linux";
})();

const hint = document.getElementById("config-path-hint");
if (hint) {
  hint.innerHTML = `<code>${paths[platform]}</code>`;
}

let currentConfig = null;

function setFormValues(form, values) {
  for (const [name, value] of Object.entries(values)) {
    const input = form.elements.namedItem(name);
    if (!input) continue;
    if (input.type === "checkbox") {
      input.checked = !!value;
    } else {
      input.value = value;
    }
  }
}

function readForm(form) {
  const out = {};
  for (const el of form.elements) {
    if (!el.name) continue;
    if (el.type === "checkbox") {
      out[el.name] = el.checked;
    } else if (el.type === "number") {
      out[el.name] = parseFloat(el.value);
    } else {
      out[el.name] = el.value;
    }
  }
  return out;
}

async function loadCapability() {
  if (!invoke) return;
  try {
    const cap = await invoke("cmd_get_capability_info");
    if (cap.degraded_notice || !cap.tesseract_available) {
      const banner = document.getElementById("capability-banner");
      const text = document.getElementById("capability-text");
      const parts = [];
      if (cap.degraded_notice) parts.push(cap.degraded_notice);
      if (!cap.tesseract_available) {
        parts.push(
          "Tesseract isn't installed — budget mode will fall back to mock OCR (empty text). Install from https://github.com/UB-Mannheim/tesseract/wiki (Windows), `brew install tesseract` (macOS), or your package manager."
        );
      }
      text.textContent = parts.join(" ");
      banner.hidden = false;
    }
  } catch (_) {}
}

async function loadCost() {
  const body = document.getElementById("cost-body");
  if (!invoke) {
    body.innerHTML = '<p class="muted">Open via <code>tauri dev</code> for live cost projection.</p>';
    return;
  }
  try {
    const proj = await invoke("cmd_get_cost_projection");
    const stale = proj.pricing_stale
      ? `<span style="color:#dc2626;"> · pricing ${proj.pricing_age_days}d old (stale)</span>`
      : "";
    body.innerHTML = `
      <div class="status-row"><span class="label">Stage 2 (per-cluster summaries)</span><span>$${proj.stage2_cost_usd.toFixed(4)}/cycle</span></div>
      <div class="status-row"><span class="label">Stage 3 (synthesis)</span><span>$${proj.stage3_cost_usd.toFixed(4)}/cycle</span></div>
      <div class="status-row"><span class="label">Total per cycle</span><span><strong>$${proj.total_cost_usd.toFixed(2)}</strong></span></div>
      <div class="status-row"><span class="label">Estimated monthly</span><span>$${proj.monthly_cost_usd.toFixed(2)} (~${proj.estimated_cycles_per_month} cycles)${stale}</span></div>
    `;
  } catch (e) {
    body.innerHTML = `<p class="muted">Cost projection unavailable: ${e}</p>`;
  }
}

async function loadSettings() {
  if (!invoke) return;
  try {
    currentConfig = await invoke("cmd_get_settings");
    const captureForm = document.getElementById("capture-form");
    const analysisForm = document.getElementById("analysis-form");
    setFormValues(captureForm, {
      cadence_minutes: currentConfig.capture.cadence_minutes,
      budget_mode: currentConfig.capture.budget_mode,
      idle_threshold_minutes: currentConfig.capture.idle_threshold_minutes,
    });
    setFormValues(analysisForm, {
      model_cluster_summary: currentConfig.analysis.model_cluster_summary,
      model_synthesis: currentConfig.analysis.model_synthesis,
      active_hours_threshold: currentConfig.analysis.active_hours_threshold,
      cost_ceiling_per_cycle_usd: currentConfig.analysis.cost_ceiling_per_cycle_usd,
      confidence_suppression_threshold: currentConfig.analysis.confidence_suppression_threshold,
    });
  } catch (e) {
    console.error("settings load failed", e);
  }
}

async function save() {
  if (!invoke || !currentConfig) return;
  const status = document.getElementById("save-status");
  status.textContent = "saving...";
  try {
    const captureValues = readForm(document.getElementById("capture-form"));
    const analysisValues = readForm(document.getElementById("analysis-form"));
    const merged = {
      ...currentConfig,
      capture: { ...currentConfig.capture, ...captureValues },
      analysis: { ...currentConfig.analysis, ...analysisValues },
    };
    await invoke("cmd_update_settings", { newConfig: merged });
    currentConfig = merged;
    status.textContent = "saved.";
    loadCost();
    setTimeout(() => (status.textContent = ""), 3000);
  } catch (e) {
    status.textContent = `failed: ${e}`;
  }
}

document.getElementById("save-btn")?.addEventListener("click", save);

loadCapability();
loadCost();
loadSettings();
