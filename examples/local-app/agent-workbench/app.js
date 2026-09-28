(function () {
  "use strict";

  const form = document.getElementById("request-form");
  const text = document.getElementById("request-text");
  const status = document.getElementById("request-status");
  const result = document.getElementById("request-result");
  const eventLog = document.getElementById("event-log");

  function setStatus(message, kind) {
    status.textContent = message;
    status.dataset.kind = kind || "";
  }

  function addEvent(event) {
    const item = document.createElement("li");
    const detail = event.event || {};
    item.textContent = [
      detail.type || "holon_event",
      detail.request_id ? "request " + detail.request_id : "",
    ].filter(Boolean).join(" — ");
    eventLog.prepend(item);
  }

  async function loadContext() {
    const context = await Holon.context();
    document.getElementById("agent-id").textContent = context.agent_id;
    document.getElementById("app-id").textContent = context.app_id;
    document.getElementById("sdk-version").textContent = context.sdk_version;
    setStatus("Connected to the owning Agent.", "ok");
  }

  form.addEventListener("submit", async function (event) {
    event.preventDefault();
    const message = text.value.trim();
    if (!message) {
      return;
    }

    const requestId = "workbench-" + Date.now().toString(36);
    setStatus("Request is being processed…", "pending");
    result.textContent = "";
    try {
      const response = await Holon.request("message", { text: message }, requestId);
      result.textContent = JSON.stringify(response, null, 2);
      setStatus("Request accepted by the Agent.", "ok");
    } catch (error) {
      result.textContent = error.message;
      setStatus("The Agent request failed.", "error");
    }
  });

  loadContext().catch(function (error) {
    setStatus("Unable to load Agent context.", "error");
    result.textContent = error.message;
  });

  Holon.events(addEvent, function () {
    setStatus("Event stream disconnected; refresh to reconnect.", "error");
  });
})();
