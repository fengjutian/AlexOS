document.querySelector("#system-info").addEventListener("click", async () => {
  const info = await window.alex.invoke("system.info");
  document.querySelector("#status").textContent = JSON.stringify(info);
});
document.querySelector("#backend").addEventListener("click", async () => {
  const result = await window.alex.invoke("runtime.invoke", {
    method: "hello.greet",
    params: { name: "WebView" },
  });
  document.querySelector("#status").textContent = result.message;
});
document.querySelector("#rename").addEventListener("click", async () => {
  await window.alex.invoke("window.setTitle", { title: "Alex OS Native Window" });
});
document.querySelector("#notify").addEventListener("click", async () => {
  await window.alex.invoke("notification.show", {
    title: "Alex OS",
    body: "Native notification from the secure Alex API",
  });
});
window.alex.on("window.focusChanged", ({ focused }) => {
  document.querySelector("#status").textContent = focused
    ? "Window focused"
    : "Window blurred";
});
