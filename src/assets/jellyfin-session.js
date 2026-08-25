// Private Jellyfin Web controller for Foreseer Desktop (protocol v1).
// Receives bootstrap/play over jellium:extension-message; never exposes tokens to Foreseer UI.
(function capturePlaybackManagerFromInputPlugin() {
  "use strict";
  function wrapInputPluginClass() {
    const Original = window._inputPlugin;
    if (!Original || Original.__foreseerWrapped) {
      return Boolean(Original && Original.__foreseerWrapped);
    }
    class ForeseerInputPlugin extends Original {
      constructor(args) {
        super(args);
        if (args && args.playbackManager) {
          window._jelliumPlaybackManager = args.playbackManager;
        }
      }
    }
    ForeseerInputPlugin.__foreseerWrapped = true;
    window._inputPlugin = ForeseerInputPlugin;
    return true;
  }
  if (!wrapInputPluginClass()) {
    const id = window.setInterval(() => {
      if (wrapInputPluginClass()) window.clearInterval(id);
    }, 50);
    window.setTimeout(() => window.clearInterval(id), 10000);
  }
})();

(function installForeseerPrivateSession() {
  "use strict";
  let attempts = 0;
  let timer;
  let generation;
  let validationGeneration;
  let externalPlayGeneration = 0;
  let pendingPlayItemId = null;

  function post(payload) {
    try {
      window.jmpNative.extensionPostMessage(JSON.stringify(payload));
    } catch (_) {}
  }

  function normalizeAddress(value) {
    return String(value || "").replace(/\/$/, "");
  }

  function persistBootstrapServer(bootstrap) {
    try {
      const key = "jellyfin_credentials";
      const raw = window.localStorage.getItem(key);
      const creds = raw ? JSON.parse(raw) : {};
      if (!creds || typeof creds !== "object") return;
      if (!Array.isArray(creds.Servers)) creds.Servers = [];
      const expected = normalizeAddress(bootstrap.serverUrl);
      const matchesUrl = (server) =>
        [server.ManualAddress, server.LocalAddress, server.RemoteAddress]
          .filter(Boolean)
          .some((address) => normalizeAddress(address) === expected);
      let server = creds.Servers.find(matchesUrl);
      if (!server) {
        server = {
          ManualAddress: expected,
          manualAddressOnly: true,
          LastConnectionMode: 2,
        };
        creds.Servers.unshift(server);
      }
      server.Id = bootstrap.serverId;
      server.AccessToken = bootstrap.accessToken;
      server.UserId = bootstrap.userId;
      server.DateLastAccessed = Date.now();
      window.localStorage.setItem(key, JSON.stringify(creds));
    } catch (_) {}
  }

  function adoptBootstrapClient(client, bootstrap) {
    persistBootstrapServer(bootstrap);
    client.serverAddress(bootstrap.serverUrl);
    if (typeof client.serverId === "function") {
      client.serverId(bootstrap.serverId);
    }
    if (typeof client.deviceId === "function" && bootstrap.deviceId) {
      client.deviceId(bootstrap.deviceId);
    }
    if (typeof client.setAuthenticationInfo === "function") {
      client.setAuthenticationInfo(bootstrap.accessToken, bootstrap.userId);
      return;
    }
    if (typeof client.userId === "function") {
      client.userId(bootstrap.userId);
    }
    if (typeof client.accessToken === "function") {
      client.accessToken(bootstrap.accessToken);
    }
  }

  function acknowledge(bootstrap, client) {
    const currentUserId =
      typeof client.getCurrentUserId === "function" ? client.getCurrentUserId() : client.userId();
    const normalizedAddress = normalizeAddress(client.serverAddress());
    const expectedAddress = normalizeAddress(bootstrap.serverUrl);
    const matches =
      normalizedAddress === expectedAddress &&
      client.serverId() === bootstrap.serverId &&
      currentUserId === bootstrap.userId &&
      Boolean(client.deviceId()) &&
      client.accessToken() === bootstrap.accessToken;
    if (!matches) return false;
    post({
      type: "session.ready",
      serverId: bootstrap.serverId,
      userId: bootstrap.userId,
      generation: bootstrap.generation,
    });
    delete window.__foreseerSessionBootstrap;
    if (timer) window.clearInterval(timer);
    if (pendingPlayItemId) {
      const itemId = pendingPlayItemId;
      pendingPlayItemId = null;
      playItem(itemId);
    }
    return true;
  }

  function applyBootstrap() {
    const bootstrap = window.__foreseerSessionBootstrap;
    const client = window.ApiClient;
    if (!bootstrap || !client) return;
    if (generation !== bootstrap.generation) {
      generation = bootstrap.generation;
      attempts = 0;
    }
    attempts += 1;
    if (attempts >= 120) {
      if (timer) window.clearInterval(timer);
      post({ type: "session.failed", generation: bootstrap.generation });
      delete window.__foreseerSessionBootstrap;
      return;
    }
    try {
      const required = ["serverAddress", "serverId", "deviceId", "accessToken"];
      if (!required.every((name) => typeof client[name] === "function")) return;
      if (!window._jelliumPlaybackManager || typeof window._jelliumPlaybackManager.play !== "function") {
        return;
      }
      if (
        typeof client.setAuthenticationInfo === "function" &&
        typeof client.getCurrentUser === "function" &&
        typeof client.getCurrentUserId === "function"
      ) {
        const hasExpectedIdentity =
          client.getCurrentUserId() === bootstrap.userId &&
          client.accessToken() === bootstrap.accessToken &&
          client.serverId() === bootstrap.serverId;
        if (!hasExpectedIdentity) {
          // Private webview: Foreseer redeem already proved this origin+token.
          // After a Jellyfin reinstall, cached credentials keep the old server
          // Id and ConnectionManager sits in ServerMismatch (not /login).
          adoptBootstrapClient(client, bootstrap);
        }
        if (validationGeneration === bootstrap.generation) return;
        validationGeneration = bootstrap.generation;
        void Promise.resolve(client.getCurrentUser())
          .then((user) => {
            validationGeneration = undefined;
            const current = window.__foreseerSessionBootstrap;
            if (!current || current.generation !== bootstrap.generation || user?.Id !== bootstrap.userId) {
              return;
            }
            acknowledge(bootstrap, client);
          })
          .catch(() => {
            validationGeneration = undefined;
          });
        return;
      }
      if (typeof client.userId !== "function") return;
      adoptBootstrapClient(client, bootstrap);
      acknowledge(bootstrap, client);
    } catch (_) {}
  }

  function playItem(itemId) {
    const apiClient = window.ApiClient;
    const playbackManager = window._jelliumPlaybackManager;
    const serverId = apiClient?.serverId?.();
    const userId = apiClient?.getCurrentUserId?.();
    if (!serverId || !userId || !playbackManager?.play) {
      post({ type: "playback.stopped" });
      return;
    }
    const playGen = ++externalPlayGeneration;
    const play = (options) => Promise.resolve(playbackManager.play(options));
    const playWithoutResume = () => play({ ids: [itemId], serverId });
    const itemRequest = typeof apiClient.getItem === "function" ? apiClient.getItem(userId, itemId) : null;
    if (!itemRequest) {
      playWithoutResume().catch(() => post({ type: "playback.stopped" }));
      return;
    }
    Promise.resolve(itemRequest)
      .then((item) => {
        if (playGen !== externalPlayGeneration) return;
        if (!item?.Id) return playWithoutResume();
        return play({
          ids: [itemId],
          serverId,
          startPositionTicks: item.UserData?.PlaybackPositionTicks || 0,
        });
      }, () => {
        if (playGen !== externalPlayGeneration) return;
        return playWithoutResume();
      })
      .catch(() => {
        if (playGen !== externalPlayGeneration) return;
        post({ type: "playback.stopped" });
      });
  }

  function clearSession() {
    delete window.__foreseerSessionBootstrap;
    pendingPlayItemId = null;
    externalPlayGeneration += 1;
    if (window._jelliumPlaybackManager?.stop) {
      void Promise.resolve(window._jelliumPlaybackManager.stop()).catch(() => {});
    }
    const client = window.ApiClient;
    if (!client) return;
    if (typeof client.clearAuthenticationInfo === "function") {
      client.clearAuthenticationInfo();
      return;
    }
    if (typeof client.logout === "function") {
      void Promise.resolve(client.logout()).catch(() => {});
    }
  }

  window.addEventListener("jellium:extension-message", function (ev) {
    let detail = ev.detail;
    if (typeof detail === "string") {
      try {
        detail = JSON.parse(detail);
      } catch (_) {
        return;
      }
    }
    if (!detail || typeof detail !== "object" || typeof detail.type !== "string") return;
    if (detail.type === "session.bootstrap") {
      window.__foreseerSessionBootstrap = {
        serverUrl: detail.serverUrl,
        serverId: detail.serverId,
        userId: detail.userId,
        deviceId: detail.deviceId,
        accessToken: detail.accessToken,
        generation: detail.bootstrapGeneration,
      };
      attempts = 0;
      if (!timer) timer = window.setInterval(applyBootstrap, 250);
      applyBootstrap();
      return;
    }
    if (detail.type === "session.clear") {
      clearSession();
      return;
    }
    if (detail.type === "play.item" && typeof detail.itemId === "string") {
      if (!window.__foreseerSessionBootstrap && window.ApiClient?.accessToken?.()) {
        playItem(detail.itemId);
      } else {
        pendingPlayItemId = detail.itemId;
        applyBootstrap();
      }
    }
  });

  if (!timer) timer = window.setInterval(applyBootstrap, 250);
})();
