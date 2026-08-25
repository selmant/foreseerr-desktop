// Foreseer Native protocol v1 bridge. Talks only through jmpNative.extensionPostMessage.
(function installForeseerNative() {
  "use strict";
  const isSetupDocument = window.location.protocol === "data:";
  const REQUEST_ID = /^[A-Za-z0-9_-]{1,64}$/;
  const ITEM_ID = /^[A-Za-z0-9_-]{1,128}$/;
  const TICKET = /^[A-Za-z0-9_-]{43}$/;

  function post(command) {
    try {
      if (!window.jmpNative || typeof window.jmpNative.extensionPostMessage !== "function") {
        return false;
      }
      window.jmpNative.extensionPostMessage(JSON.stringify(command));
      return true;
    } catch (_) {
      return false;
    }
  }

  function showRuntimeRecovery(message) {
    if (isSetupDocument) return;
    document.title = "Foreseer Recovery";
    document.body.replaceChildren();
    const root = document.createElement("main");
    root.style.cssText = "max-width:42rem;margin:12vh auto;padding:2rem;font-family:system-ui,sans-serif;line-height:1.5";
    const heading = document.createElement("h1");
    heading.textContent = "Foreseer needs to restart";
    const detail = document.createElement("p");
    detail.textContent = message || "The bundled Foreseerr server stopped responding.";
    const hint = document.createElement("p");
    hint.textContent = "Close and reopen Foreseer to start the local server again. Your standalone data was not removed.";
    const quit = document.createElement("button");
    quit.type = "button";
    quit.textContent = "Quit";
    quit.addEventListener("click", function () {
      api.send({ type: "app.quit", id: crypto.randomUUID() });
    });
    const retry = document.createElement("button");
    retry.type = "button";
    retry.textContent = "Retry";
    retry.addEventListener("click", function () {
      retry.disabled = true;
      api.send({ type: "runtime.retry", id: crypto.randomUUID() });
    });
    const logs = document.createElement("button");
    logs.type = "button";
    logs.textContent = "Open Logs";
    logs.addEventListener("click", function () {
      api.send({ type: "runtime.open-logs", id: crypto.randomUUID() });
    });
    const remote = document.createElement("button");
    remote.type = "button";
    remote.textContent = "Use Remote Mode";
    remote.addEventListener("click", function () {
      remote.disabled = true;
      api.send({ type: "runtime.open-setup", id: crypto.randomUUID() });
    });
    root.append(heading, detail, hint, retry, logs, remote, quit);
    document.body.append(root);
  }

  function isLoginPath() {
    const path = window.location.pathname || "";
    return path === "/login" || path.indexOf("/login/") === 0;
  }

  function removeLoginRemoteChrome() {
    const existing = document.getElementById("foreseer-login-remote");
    if (existing) existing.remove();
  }

  function showLoginRemoteChrome() {
    if (isSetupDocument || !isLoginPath()) {
      removeLoginRemoteChrome();
      return;
    }
    if (!api.capabilities.includes("mode-setup")) return;
    if (document.querySelector("[data-foreseer-remote-setup]")) {
      removeLoginRemoteChrome();
      return;
    }
    if (document.getElementById("foreseer-login-remote")) return;
    if (!document.body) return;
    const bar = document.createElement("div");
    bar.id = "foreseer-login-remote";
    bar.style.cssText =
      "position:fixed;left:50%;bottom:1.5rem;transform:translateX(-50%);z-index:2147483646;width:min(24rem,calc(100vw - 2rem));padding:1rem 1.25rem;border-radius:0.75rem;background:rgba(17,24,39,0.92);border:1px solid rgba(75,85,99,0.9);box-shadow:0 10px 40px rgba(0,0,0,0.45);font-family:system-ui,sans-serif;text-align:center;color:#e5e7eb";
    const label = document.createElement("p");
    label.style.cssText = "margin:0 0 0.75rem;font-size:0.875rem;line-height:1.4;color:#d1d5db";
    label.textContent = "This app is using the local Foreseer server on this computer.";
    const button = document.createElement("button");
    button.type = "button";
    button.textContent = "Use a remote Foreseerr instead";
    button.style.cssText =
      "width:100%;padding:0.6rem 0.75rem;border-radius:0.5rem;border:1px solid #4b5563;background:transparent;color:#f3f4f6;font-size:0.875rem;cursor:pointer";
    button.addEventListener("click", function () {
      button.disabled = true;
      api.send({ type: "runtime.open-setup", id: crypto.randomUUID() });
    });
    bar.append(label, button);
    document.body.append(bar);
  }

  function hookHistory(method) {
    const original = history[method];
    if (typeof original !== "function") return;
    history[method] = function () {
      const result = original.apply(this, arguments);
      queueMicrotask(showLoginRemoteChrome);
      return result;
    };
  }

  const api = {
    protocolVersion: 1,
    hostName: "foreseer-desktop",
    hostVersion: "__HOST_VERSION__",
    capabilities: Object.freeze(
      isSetupDocument
        ? ["setup", "window-controls", "quit"]
        : [
            "play-item",
            "auth-bootstrap",
            "player-events",
            "session-reset",
            "browser-cache-clear",
            "mode-setup",
            "window-controls",
            "quit",
          ]
    ),
    send(command) {
      if (!command || typeof command !== "object" || typeof command.type !== "string") {
        return false;
      }
      if (typeof command.id !== "string" || !REQUEST_ID.test(command.id)) {
        return false;
      }
      switch (command.type) {
        case "auth.challenge":
        case "session.clear":
        case "runtime.retry":
        case "runtime.open-logs":
        case "runtime.open-setup":
        case "window.minimize":
        case "window.toggle-maximize":
        case "window.toggle-fullscreen":
        case "app.quit":
          return post(command);
        case "auth.complete":
        case "browser-cache.clear":
          return typeof command.ticket === "string" && TICKET.test(command.ticket)
            ? post(command)
            : false;
        case "play.item":
          return typeof command.itemId === "string" && ITEM_ID.test(command.itemId)
            ? post(command)
            : false;
        case "setup.check":
        case "setup.save":
          if (!isSetupDocument) return false;
          return typeof command.url === "string" && typeof command.allowHttp === "boolean"
            ? post(command)
            : false;
        default:
          return false;
      }
    },
  };

  Object.defineProperty(window, "foreseerNative", {
    configurable: false,
    enumerable: true,
    writable: false,
    value: Object.freeze(api),
  });

  hookHistory("pushState");
  hookHistory("replaceState");
  window.addEventListener("popstate", showLoginRemoteChrome);
  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", showLoginRemoteChrome);
  } else {
    showLoginRemoteChrome();
  }
  window.addEventListener("load", showLoginRemoteChrome);

  window.addEventListener("jellium:extension-message", function (ev) {
    let detail = ev.detail;
    if (typeof detail === "string") {
      try {
        detail = JSON.parse(detail);
      } catch (_) {
        return;
      }
    }
    if (!detail || typeof detail !== "object") return;
    if (detail.type === "runtime-failed") {
      showRuntimeRecovery(detail.message);
    }
    if (detail.type === "runtime-recovered") {
      window.location.reload();
    }
    if (detail.type === "auth-challenge" || detail.type === "error") {
      console.info("[ForeseerNative] host event", detail.type, detail.id);
    }
    window.dispatchEvent(
      new CustomEvent("foreseer:native-event", {
        detail: Object.freeze(detail),
      })
    );
  });
})();
