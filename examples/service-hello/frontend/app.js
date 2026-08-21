document.querySelector("#rename").addEventListener("click", async () => {
  await window.alex.invoke("window.setTitle", { title: "Alex OS Service Demo" });
});
document.querySelector("#notify").addEventListener("click", async () => {
  await window.alex.invoke("notification.show", {
    title: "Alex OS",
    body: "Service-mode demo (see backend stderr for ready signal).",
  });
});
window.alex.on("window.focusChanged", ({ focused }) => {
  document.querySelector("#status").textContent = focused
    ? "Window focused"
    : "Window blurred";
});
