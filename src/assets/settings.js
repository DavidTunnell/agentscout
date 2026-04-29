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

// ───────────────────────────────────────────────────────────────────────
// v0.5.5 — Anthropic API key entry + test
// ───────────────────────────────────────────────────────────────────────

async function refreshApiKeyStatus() {
  const status = document.getElementById("apikey-status");
  if (!status || !invoke) return;
  try {
    const creds = await invoke("cmd_get_credentials_status");
    if (creds.has_anthropic_key) {
      status.innerHTML = '<span style="color: #16a34a;">&#10003; Anthropic key set in keychain.</span>';
    } else {
      status.innerHTML = '<span style="color: #dc2626;">&#10007; No Anthropic key set. Paste your key below.</span>';
    }
  } catch (e) {
    status.textContent = `Status check failed: ${e}`;
  }
}

async function saveApiKey() {
  if (!invoke) return;
  const input = document.getElementById("apikey-input");
  const result = document.getElementById("apikey-result");
  const key = input.value.trim();
  if (!key) {
    result.innerHTML = '<span style="color: #dc2626;">Paste a key first.</span>';
    return;
  }
  result.textContent = "Saving...";
  try {
    await invoke("cmd_set_anthropic_key", { key });
    input.value = "";
    result.innerHTML = '<span style="color: #16a34a;">&#10003; Saved to keychain.</span>';
    refreshApiKeyStatus();
  } catch (e) {
    result.innerHTML = `<span style="color: #dc2626;">Save failed: ${e}</span>`;
  }
}

async function testApiKey() {
  if (!invoke) return;
  const result = document.getElementById("apikey-result");
  result.textContent = "Testing connection (this calls api.anthropic.com)...";
  try {
    const res = await invoke("cmd_test_anthropic_key");
    if (res.ok) {
      result.innerHTML = `<span style="color: #16a34a;">&#10003; ${res.message}</span>`;
    } else {
      result.innerHTML = `<span style="color: #dc2626;">&#10007; ${res.message}</span>`;
    }
  } catch (e) {
    result.innerHTML = `<span style="color: #dc2626;">Test failed: ${e}</span>`;
  }
}

async function clearApiKey() {
  if (!invoke) return;
  const result = document.getElementById("apikey-result");
  if (!confirm("Clear the stored Anthropic API key? Analysis cycles will fail until you set a new one.")) return;
  try {
    await invoke("cmd_clear_anthropic_key");
    result.innerHTML = '<span style="color: #16a34a;">&#10003; Cleared.</span>';
    refreshApiKeyStatus();
  } catch (e) {
    result.innerHTML = `<span style="color: #dc2626;">Clear failed: ${e}</span>`;
  }
}

document.getElementById("apikey-save-btn")?.addEventListener("click", saveApiKey);
document.getElementById("apikey-test-btn")?.addEventListener("click", testApiKey);
document.getElementById("apikey-clear-btn")?.addEventListener("click", clearApiKey);
// Enter inside the input triggers Save.
document.getElementById("apikey-input")?.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    e.preventDefault();
    saveApiKey();
  }
});

// ───────────────────────────────────────────────────────────────────────
// v0.5.6 — Setup + Tier Calibration conversations
// ───────────────────────────────────────────────────────────────────────

function appendChatMessage(windowEl, role, text) {
  const div = document.createElement("div");
  div.className = `chat-msg ${role}`;
  const roleLabel = document.createElement("div");
  roleLabel.className = "role-label";
  roleLabel.textContent = role === "assistant" ? "AgentScout" : "You";
  const body = document.createElement("div");
  body.textContent = text;
  div.appendChild(roleLabel);
  div.appendChild(body);
  windowEl.appendChild(div);
  windowEl.scrollTop = windowEl.scrollHeight;
}

async function refreshPersonalizationStatus() {
  if (!invoke) return;
  const el = document.getElementById("personalization-status");
  if (!el) return;
  try {
    const p = await invoke("cmd_get_personalization_status");
    const lines = [];
    lines.push(
      p.has_user_profile
        ? '<span style="color:#16a34a;">&#10003; user-profile.md exists</span>'
        : '<span style="color:#a16207;">&#9888; user-profile.md not generated yet</span>'
    );
    lines.push(
      p.has_tier_definitions
        ? '<span style="color:#16a34a;">&#10003; tier-definitions.json exists</span>'
        : '<span style="color:#a16207;">&#9888; tier-definitions.json not generated yet</span>'
    );
    if (p.user_profile_excerpt) {
      lines.push(
        `<details style="margin-top: 8px;"><summary class="muted">Profile preview</summary>` +
        `<pre style="white-space: pre-wrap; font-size: 12px; padding: 8px; background: var(--card); border-radius: 4px;">${escapeHtml(p.user_profile_excerpt)}...</pre></details>`
      );
    }
    el.innerHTML = lines.join("<br>");
  } catch (e) {
    el.textContent = `Status check failed: ${e}`;
  }
}

function escapeHtml(s) {
  return (s || "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

// Generic chat-conversation wiring used for both setup and tier-cal.
function wireConversation(opts) {
  const startBtn = document.getElementById(opts.startBtnId);
  const sendBtn = document.getElementById(opts.sendBtnId);
  const finalizeBtn = document.getElementById(opts.finalizeBtnId);
  const inputEl = document.getElementById(opts.inputId);
  const inputRow = document.getElementById(opts.inputRowId);
  const chatEl = document.getElementById(opts.chatId);
  const templateSel = document.getElementById(opts.templateSelId);

  startBtn?.addEventListener("click", async () => {
    if (!invoke) return;
    chatEl.innerHTML = "";
    chatEl.hidden = false;
    inputRow.hidden = false;
    finalizeBtn.hidden = false;
    startBtn.disabled = true;
    try {
      const opener = await invoke(opts.startCmd, { templateId: templateSel.value });
      appendChatMessage(chatEl, "assistant", opener);
    } catch (e) {
      appendChatMessage(chatEl, "assistant", `Failed to start: ${e}`);
      startBtn.disabled = false;
    }
  });

  async function send() {
    if (!invoke) return;
    const reply = inputEl.value.trim();
    if (!reply) return;
    appendChatMessage(chatEl, "user", reply);
    inputEl.value = "";
    sendBtn.disabled = true;
    try {
      const step = await invoke(opts.continueCmd, { reply });
      appendChatMessage(chatEl, "assistant", step.bot_message);
    } catch (e) {
      appendChatMessage(chatEl, "assistant", `Error: ${e}`);
    } finally {
      sendBtn.disabled = false;
      inputEl.focus();
    }
  }

  sendBtn?.addEventListener("click", send);
  inputEl?.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
    }
  });

  finalizeBtn?.addEventListener("click", async () => {
    if (!invoke) return;
    finalizeBtn.disabled = true;
    appendChatMessage(chatEl, "user", "(finalize and write the file)");
    try {
      const path = await invoke(opts.finalizeCmd);
      appendChatMessage(chatEl, "assistant", `&#10003; Wrote ${path}. You can re-run this conversation any time.`);
      // Reset for re-run
      startBtn.disabled = false;
      finalizeBtn.disabled = true;
      finalizeBtn.hidden = true;
      inputRow.hidden = true;
      refreshPersonalizationStatus();
    } catch (e) {
      appendChatMessage(chatEl, "assistant", `Finalize failed: ${e}`);
      finalizeBtn.disabled = false;
    }
  });
}

wireConversation({
  startBtnId: "setup-start-btn",
  sendBtnId: "setup-send-btn",
  finalizeBtnId: "setup-finalize-btn",
  inputId: "setup-input",
  inputRowId: "setup-input-row",
  chatId: "setup-chat",
  templateSelId: "setup-template",
  startCmd: "cmd_start_setup_conversation",
  continueCmd: "cmd_continue_setup_conversation",
  finalizeCmd: "cmd_finalize_setup_conversation",
});

wireConversation({
  startBtnId: "tier-start-btn",
  sendBtnId: "tier-send-btn",
  finalizeBtnId: "tier-finalize-btn",
  inputId: "tier-input",
  inputRowId: "tier-input-row",
  chatId: "tier-chat",
  templateSelId: "tier-template",
  startCmd: "cmd_start_tier_calibration",
  continueCmd: "cmd_continue_tier_calibration",
  finalizeCmd: "cmd_finalize_tier_calibration",
});

// ───────────────────────────────────────────────────────────────────────
// v0.5.7 — Gmail OAuth + recipient + test email
// ───────────────────────────────────────────────────────────────────────

async function refreshGmailStatus() {
  if (!invoke) return;
  const el = document.getElementById("gmail-status");
  const redirectEl = document.getElementById("gmail-redirect-uri-display");
  if (!el) return;
  try {
    const c = await invoke("cmd_get_credentials_status");
    const cycleStatus = await invoke("cmd_get_cycle_status");
    if (redirectEl) redirectEl.textContent = `${cycleStatus.disposition_server_origin}/oauth/callback`;
    const lines = [];
    if (c.has_gmail_oauth) {
      lines.push('<span style="color:#16a34a;">&#10003; Gmail connected (OAuth client + refresh token).</span>');
    } else {
      lines.push('<span style="color:#a16207;">&#9888; Gmail not fully connected.</span>');
    }
    if (c.recipient_email) {
      lines.push(`<span class="muted">Recipient: ${escapeHtml(c.recipient_email)}</span>`);
    }
    el.innerHTML = lines.join("<br>");
    document.getElementById("gmail-recipient").value = c.recipient_email || "";
  } catch (e) {
    el.textContent = `Status check failed: ${e}`;
  }
}

async function saveGmailCreds() {
  const result = document.getElementById("gmail-result");
  const cid = document.getElementById("gmail-client-id").value.trim();
  const csec = document.getElementById("gmail-client-secret").value.trim();
  if (!cid) {
    result.innerHTML = '<span style="color:#dc2626;">client_id required.</span>';
    return;
  }
  result.textContent = "Saving creds...";
  try {
    await invoke("cmd_set_gmail_oauth_creds", {
      args: { client_id: cid, client_secret: csec || null },
    });
    document.getElementById("gmail-client-secret").value = "";
    result.innerHTML = '<span style="color:#16a34a;">&#10003; Saved. Now click Connect Gmail.</span>';
    refreshGmailStatus();
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Save failed: ${e}</span>`;
  }
}

async function clearGmailCreds() {
  if (!confirm("Clear Gmail OAuth creds and revoke the stored refresh token?")) return;
  const result = document.getElementById("gmail-result");
  try {
    await invoke("cmd_clear_gmail_oauth_creds");
    result.innerHTML = '<span style="color:#16a34a;">&#10003; Gmail creds + refresh token cleared.</span>';
    refreshGmailStatus();
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Clear failed: ${e}</span>`;
  }
}

async function connectGmail() {
  const result = document.getElementById("gmail-result");
  result.textContent = "Starting OAuth flow...";
  try {
    const begin = await invoke("cmd_begin_gmail_oauth");
    // Use the Tauri shell plugin's open() to launch the browser. The
    // user authenticates with Google, Google redirects to our local
    // disposition server, we store the refresh token, and the user
    // sees a "Gmail connected" page in the browser.
    if (window.__TAURI__?.shell?.open) {
      await window.__TAURI__.shell.open(begin.auth_url);
    } else {
      // Dev/fallback path
      window.open(begin.auth_url, "_blank");
    }
    result.textContent = "Browser opened — complete consent there. Polling for completion...";
    // Poll status every 2s for up to 5 min.
    const deadline = Date.now() + 5 * 60 * 1000;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 2000));
      const status = await invoke("cmd_poll_gmail_oauth_status", { csrfState: begin.csrf_state });
      if (status.kind === "completed") {
        result.innerHTML = '<span style="color:#16a34a;">&#10003; Gmail connected. Set a recipient + click Send test email.</span>';
        refreshGmailStatus();
        return;
      }
      if (status.kind === "failed") {
        result.innerHTML = `<span style="color:#dc2626;">OAuth failed: ${escapeHtml(status.error)}</span>`;
        return;
      }
    }
    result.innerHTML = '<span style="color:#a16207;">Timed out waiting for OAuth (5 min). Try again.</span>';
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Connect failed: ${e}</span>`;
  }
}

async function disconnectGmail() {
  if (!confirm("Disconnect Gmail? You can reconnect any time.")) return;
  const result = document.getElementById("gmail-result");
  try {
    await invoke("cmd_disconnect_gmail");
    result.innerHTML = '<span style="color:#16a34a;">&#10003; Refresh token revoked.</span>';
    refreshGmailStatus();
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Disconnect failed: ${e}</span>`;
  }
}

async function saveRecipient() {
  const result = document.getElementById("gmail-result");
  const email = document.getElementById("gmail-recipient").value.trim();
  try {
    await invoke("cmd_set_recipient_email", { email });
    result.innerHTML = '<span style="color:#16a34a;">&#10003; Recipient saved.</span>';
    refreshGmailStatus();
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Save failed: ${e}</span>`;
  }
}

async function sendTestEmail() {
  const result = document.getElementById("gmail-result");
  result.textContent = "Sending test email (calls Gmail API)...";
  try {
    const msg = await invoke("cmd_send_test_email");
    result.innerHTML = `<span style="color:#16a34a;">&#10003; ${escapeHtml(msg)}</span>`;
  } catch (e) {
    result.innerHTML = `<span style="color:#dc2626;">Send failed: ${e}</span>`;
  }
}

document.getElementById("gmail-creds-save-btn")?.addEventListener("click", saveGmailCreds);
document.getElementById("gmail-creds-clear-btn")?.addEventListener("click", clearGmailCreds);
document.getElementById("gmail-connect-btn")?.addEventListener("click", connectGmail);
document.getElementById("gmail-disconnect-btn")?.addEventListener("click", disconnectGmail);
document.getElementById("gmail-recipient-save-btn")?.addEventListener("click", saveRecipient);
document.getElementById("gmail-test-email-btn")?.addEventListener("click", sendTestEmail);

loadCapability();
loadCost();
loadSettings();
refreshApiKeyStatus();
refreshPersonalizationStatus();
refreshGmailStatus();
