const OVERLAY_STATE_KEY = "foundry-overlay-collapsed";
const DISCONNECTED_OPACITY = "0.5";

export function createGuiController({
  overlay,
  overlayToggle,
  logList,
  endpointEl,
  canvas,
} = {}) {
  initOverlay();

  function log(...args) {
    console.log(...args);
    if (!logList) return;
    const li = document.createElement("li");
    li.textContent = args.join(" ");
    logList.appendChild(li);
    logList.scrollTop = logList.scrollHeight;
  }

  function setEndpoint(endpoint) {
    if (endpointEl) {
      endpointEl.textContent = endpoint;
    }
  }

  function setCanvasConnected(isConnected, opacity = DISCONNECTED_OPACITY) {
    if (!canvas) return;
    canvas.style.opacity = isConnected ? "1" : opacity;
  }

  function initOverlay() {
    if (!overlay || !overlayToggle) return;
    const collapsed = loadOverlayState();
    setOverlayCollapsed(collapsed);
    overlayToggle.addEventListener("click", () => {
      const next = !overlay.classList.contains("collapsed");
      setOverlayCollapsed(next);
      persistOverlayState(next);
    });
  }

  function setOverlayCollapsed(collapsed) {
    if (!overlay || !overlayToggle) return;
    overlay.classList.toggle("collapsed", collapsed);
    overlayToggle.textContent = collapsed ? "Show panel" : "Hide panel";
    overlayToggle.setAttribute("aria-expanded", (!collapsed).toString());
  }

  function loadOverlayState() {
    try {
      return localStorage.getItem(OVERLAY_STATE_KEY) === "1";
    } catch (_) {
      return false;
    }
  }

  function persistOverlayState(collapsed) {
    try {
      localStorage.setItem(OVERLAY_STATE_KEY, collapsed ? "1" : "0");
    } catch (_) {}
  }

  return {
    log,
    setEndpoint,
    setCanvasConnected,
  };
}
