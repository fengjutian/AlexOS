// Alex Notes frontend.
//
// All HTTP goes through `fetch("alex://app/api/…", …)`. The host's
// reverse proxy injects `X-Alx-Token` and routes the request to the
// service backend; the page never sees the upstream port and the
// loopback listener can refuse any caller that isn't the host.

const statusEl = document.querySelector("#status");
const listEl = document.querySelector("#notes");
const refreshBtn = document.querySelector("#refresh");
const formEl = document.querySelector("#note-form");
const titleEl = document.querySelector("#title");
const bodyEl = document.querySelector("#body");
const formStatusEl = document.querySelector("#form-status");

function setStatus(text, isError) {
  statusEl.textContent = text;
  statusEl.classList.toggle("error", Boolean(isError));
}

function setFormStatus(text, isError) {
  formStatusEl.textContent = text;
  formStatusEl.classList.toggle("error", Boolean(isError));
}

function formatDate(iso) {
  try {
    return new Date(iso).toLocaleString();
  } catch {
    return iso;
  }
}

function makeNoteRow(note) {
  const li = document.createElement("li");
  const header = document.createElement("div");
  header.className = "note-header";
  const title = document.createElement("span");
  title.className = "note-title";
  title.textContent = note.title;
  const date = document.createElement("span");
  date.className = "note-date";
  date.textContent = formatDate(note.createdAt);
  header.append(title, date);

  const body = document.createElement("p");
  body.className = "note-body";
  body.textContent = note.body;

  const actions = document.createElement("div");
  actions.className = "note-actions";
  const deleteBtn = document.createElement("button");
  deleteBtn.type = "button";
  deleteBtn.textContent = "Delete";
  deleteBtn.addEventListener("click", () => deleteNote(note.id, deleteBtn));
  actions.appendChild(deleteBtn);

  li.append(header, body, actions);
  return li;
}

async function loadNotes() {
  setStatus("Loading…");
  listEl.replaceChildren();
  try {
    const response = await fetch("alex://app/api/notes", {
      headers: { accept: "application/json" },
    });
    if (!response.ok) {
      setStatus(`Failed to load: ${response.status} ${response.statusText}`, true);
      return;
    }
    const payload = await response.json();
    const notes = Array.isArray(payload?.notes) ? payload.notes : [];
    if (notes.length === 0) {
      setStatus("No notes yet. Add one above.");
    } else {
      setStatus(`${notes.length} note(s) stored on the host.`);
      for (const note of notes) {
        listEl.appendChild(makeNoteRow(note));
      }
    }
  } catch (error) {
    setStatus(`Failed to load: ${error?.message ?? error}`, true);
  }
}

async function deleteNote(id, button) {
  button.disabled = true;
  try {
    const response = await fetch(`alex://app/api/notes/${id}`, {
      method: "DELETE",
    });
    if (!response.ok && response.status !== 204) {
      setStatus(`Delete failed: ${response.status} ${response.statusText}`, true);
      button.disabled = false;
      return;
    }
    await loadNotes();
  } catch (error) {
    setStatus(`Delete failed: ${error?.message ?? error}`, true);
    button.disabled = false;
  }
}

async function createNote(event) {
  event.preventDefault();
  const title = titleEl.value.trim();
  const body = bodyEl.value.trim();
  if (!title || !body) {
    setFormStatus("Both title and body are required.", true);
    return;
  }
  setFormStatus("Saving…");
  try {
    const response = await fetch("alex://app/api/notes", {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify({ title, body }),
    });
    if (!response.ok) {
      const text = await response.text();
      setFormStatus(`Save failed: ${response.status} ${text}`, true);
      return;
    }
    titleEl.value = "";
    bodyEl.value = "";
    setFormStatus("Saved.");
    await loadNotes();
  } catch (error) {
    setFormStatus(`Save failed: ${error?.message ?? error}`, true);
  }
}

refreshBtn.addEventListener("click", loadNotes);
formEl.addEventListener("submit", createNote);

async function waitForBridge() {
  if (window.alex && typeof window.alex.invoke === "function") return;
  await new Promise((resolve) => {
    const check = () => {
      if (window.alex && typeof window.alex.invoke === "function") {
        clearInterval(timer);
        resolve();
      }
    };
    const timer = setInterval(check, 25);
  });
}

(async () => {
  await waitForBridge();
  await loadNotes();
})();
