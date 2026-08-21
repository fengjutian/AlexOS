document.querySelector("#rename").addEventListener("click", async () => {
  await window.alex.invoke("window.setTitle", { title: "Alex OS Service Demo" });
});
document.querySelector("#notify").addEventListener("click", async () => {
  await window.alex.invoke("notification.show", {
    title: "Alex OS",
    body: "Service-mode demo (see backend stderr for ready signal).",
  });
});
document.querySelector("#call").addEventListener("click", async () => {
  const out = document.querySelector("#output");
  out.hidden = false;
  out.textContent = "calling /api/info via alex://app/...";
  try {
    // The page is same-origin to alex://app/, so the host's
    // reverse proxy can mediate without the page knowing the
    // loopback port the backend is listening on. The X-Alx-Token
    // header is injected by the host; the backend just sees a
    // normal HTTP request.
    const response = await fetch("alex://app/api/info", {
      headers: { accept: "application/json" },
    });
    if (!response.ok) {
      out.textContent = `proxy error: ${response.status} ${response.statusText}`;
      return;
    }
    const body = await response.json();
    out.textContent = JSON.stringify(body, null, 2);
  } catch (error) {
    out.textContent = `fetch failed: ${error?.message ?? error}`;
  }
});
window.alex.on("window.focusChanged", ({ focused }) => {
  document.querySelector("#status").textContent = focused
    ? "Window focused"
    : "Window blurred";
});
