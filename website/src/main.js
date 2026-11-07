(() => {
  "use strict";

  const menu = document.querySelector(".mobile-menu");
  if (menu) {
    menu.addEventListener("click", (event) => {
      if (event.target instanceof Element && event.target.closest("nav a")) {
        menu.open = false;
      }
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && menu.open) {
        menu.open = false;
        menu.querySelector("summary")?.focus();
      }
    });
    document.addEventListener("pointerdown", (event) => {
      if (menu.open && event.target instanceof Node && !menu.contains(event.target)) {
        menu.open = false;
      }
    });
  }

  // All product information and download fallbacks are usable without this file.
  // Optional, same-origin release metadata may refine existing public links only.
  function approvedDownloadUrl(value) {
    if (typeof value !== "string") return null;
    try {
      const url = new URL(value);
      const path = /^\/ForceMind\/Linko\/actions\/runs\/\d+(?:\/artifacts\/\d+)?\/?$/;
      return url.protocol === "https:" && url.hostname === "github.com" &&
        !url.port && !url.username && !url.password && !url.search && !url.hash && path.test(url.pathname)
        ? url.href : null;
    } catch {
      return null;
    }
  }

  async function readRelease() {
    if (!/^https?:$/.test(window.location.protocol)) return;
    const controller = new AbortController();
    const timeout = window.setTimeout(() => controller.abort(), 6000);
    try {
      const response = await fetch("./release.json", {
        cache: "no-cache",
        credentials: "omit",
        signal: controller.signal,
      });
      if (!response.ok) return;
      const release = await response.json();
      if (release?.releaseStatus !== "experimental") return;
      if (typeof release.version === "string" && /^\d+\.\d+\.\d+-dev$/.test(release.version)) {
        document.querySelectorAll("[data-release-version]").forEach((element) => {
          element.textContent = release.version;
        });
      }
      if (!Array.isArray(release.downloads)) return;
      const platforms = new Set(["macos", "windows", "linux"]);
      const updated = new Set();
      release.downloads.slice(0, 12).forEach((download) => {
        if (!download || !platforms.has(download.platform) || updated.has(download.platform)) return;
        const href = approvedDownloadUrl(download.url);
        if (!href) return;
        updated.add(download.platform);
        const card = document.querySelector(`[data-platform="${download.platform}"]`);
        const link = card?.querySelector("[data-download-link]");
        if (link) link.href = href;
        if (typeof download.sha256 === "string" && /^[a-f\d]{64}$/i.test(download.sha256)) {
          const box = card?.querySelector("[data-checksum-box]");
          const checksum = card?.querySelector("[data-checksum]");
          if (box && checksum) {
            checksum.textContent = download.sha256.toLowerCase();
            box.hidden = false;
          }
        }
      });
    } catch {
      // Keep the honest, static GitHub build-page links if metadata is unavailable.
    } finally {
      window.clearTimeout(timeout);
    }
  }

  readRelease();
})();
