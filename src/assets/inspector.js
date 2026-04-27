const invoke = window.__TAURI__?.core?.invoke;

const grid = document.getElementById("capture-grid");
const meta = document.getElementById("meta");
const detail = document.getElementById("detail-panel");
const detailBody = document.getElementById("detail-body");
const limitSelect = document.getElementById("limit-select");
const refreshBtn = document.getElementById("refresh-btn");

function fmtTime(ts) {
  return new Date(ts * 1000).toLocaleString();
}

function escapeHtml(s) {
  return (s ?? "").replace(/[&<>"']/g, (c) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[c]));
}

async function loadList() {
  if (!invoke) {
    meta.textContent = "Open through `tauri dev` to use the inspector.";
    return;
  }
  const limit = parseInt(limitSelect.value, 10);
  try {
    const rows = await invoke("cmd_list_recent_captures", { limit });
    meta.textContent = `${rows.length} captures (most recent first)`;
    grid.innerHTML = rows.map((r) => `
      <div class="capture-card" data-id="${r.id}">
        <div class="capture-thumb" data-id="${r.id}">
          <span class="muted">click to load</span>
        </div>
        <div class="capture-meta">
          <div class="capture-time">${fmtTime(r.timestamp)}</div>
          <div class="capture-app">${escapeHtml(r.foreground_app || "—")}</div>
          <div class="capture-title muted">${escapeHtml(r.foreground_window_title || "")}</div>
          ${r.ocr_engine ? `<div class="capture-ocr-tag">OCR: ${escapeHtml(r.ocr_engine)}</div>` : ""}
        </div>
      </div>
    `).join("");

    grid.querySelectorAll(".capture-card").forEach((card) => {
      card.addEventListener("click", () => loadDetail(parseInt(card.dataset.id, 10)));
    });
  } catch (e) {
    meta.textContent = `Failed to load captures: ${e}`;
  }
}

async function loadDetail(captureId) {
  if (!invoke) return;
  detail.hidden = false;
  detailBody.innerHTML = `<p class="muted">Loading capture ${captureId}...</p>`;
  try {
    const payload = await invoke("cmd_get_capture_image", { captureId });
    const src = `data:${payload.mime};base64,${payload.data_base64}`;
    detailBody.innerHTML = `
      <div class="detail-image"><img src="${src}" alt="capture ${captureId}" /></div>
      <p class="muted">${payload.from_thumbnail ? "Thumbnail (budget mode)" : "Full resolution"} • ${(payload.data_base64.length * 3 / 4 / 1024).toFixed(1)} KB</p>
      ${payload.ocr_text ? `<details open><summary>OCR text</summary><pre class="ocr-text">${escapeHtml(payload.ocr_text)}</pre></details>` : '<p class="muted">No OCR text (budget mode disabled or OCR engine unavailable).</p>'}
    `;
    detail.scrollIntoView({ behavior: "smooth", block: "start" });
  } catch (e) {
    detailBody.innerHTML = `<p class="muted">Error: ${e}</p>`;
  }
}

limitSelect.addEventListener("change", loadList);
refreshBtn.addEventListener("click", loadList);

loadList();
