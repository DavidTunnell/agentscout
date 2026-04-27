const paths = {
  windows: "%APPDATA%\\AgentScout\\config.json",
  macos: "~/Library/Application Support/AgentScout/config.json",
  linux: "~/.local/share/agentscout/config.json",
};

const platform = (() => {
  const ua = navigator.userAgent;
  if (ua.includes("Windows")) return "windows";
  if (ua.includes("Mac")) return "macos";
  return "linux";
})();

const hint = document.getElementById("config-path-hint");
if (hint) {
  hint.innerHTML = `<p class="muted">On this platform, config lives at:<br/><code>${paths[platform]}</code></p>`;
}
