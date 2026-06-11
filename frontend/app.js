const SESSION_TOKEN_KEY = "secure-chat-token";
const SESSION_USER_KEY = "secure-chat-user";

const state = {
  mode: "login",
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) || "",
  user: readStoredUser(),
  rooms: [],
  activeRoom: null,
  socket: null,
};

const $ = (selector) => document.querySelector(selector);
const authForm = $("#auth-form");
const roomForm = $("#room-form");
const messageForm = $("#message-form");
const messages = $("#messages");
const messageInput = $("#message-input");
const sendButton = $("#send-button");
const socketStatus = $("#socket-status");

function readStoredUser() {
  const storedUser = sessionStorage.getItem(SESSION_USER_KEY);
  if (!storedUser) return null;

  try {
    return JSON.parse(storedUser);
  } catch {
    sessionStorage.removeItem(SESSION_USER_KEY);
    return null;
  }
}

function apiUrl(path) {
  return `${window.location.origin}${path}`;
}

function wsUrl(roomId, token) {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const params = new URLSearchParams({ room_id: roomId, token });
  return `${protocol}//${window.location.host}/ws?${params}`;
}

function authHeaders(extraHeaders = {}) {
  return {
    ...extraHeaders,
    Authorization: `Bearer ${state.token}`,
  };
}

async function apiRequest(path, options = {}) {
  const response = await fetch(apiUrl(path), options);
  const payload = await response.json().catch(() => null);
  if (!response.ok) {
    throw new Error(payload?.error || "Request failed");
  }
  return payload;
}

function setStatus(element, label, variant = "") {
  element.textContent = label;
  element.className = `status-pill ${variant}`.trim();
}

function showToast(message, type = "success") {
  const toast = document.createElement("div");
  toast.className = `toast ${type}`;
  toast.textContent = message;
  $("#toast-region").append(toast);
  window.setTimeout(() => toast.remove(), 3600);
}

function saveSession(payload) {
  state.token = payload.token;
  state.user = payload.user;
  sessionStorage.setItem(SESSION_TOKEN_KEY, payload.token);
  sessionStorage.setItem(SESSION_USER_KEY, JSON.stringify(payload.user));
  renderSession();
  loadRooms();
}

function clearSession() {
  disconnectSocket();
  state.token = "";
  state.user = null;
  state.rooms = [];
  state.activeRoom = null;
  sessionStorage.removeItem(SESSION_TOKEN_KEY);
  sessionStorage.removeItem(SESSION_USER_KEY);
  renderSession();
}

function renderSession() {
  const loggedIn = Boolean(state.token && state.user);
  $("#auth-screen").classList.toggle("hidden", loggedIn);
  $("#app-screen").classList.toggle("hidden", !loggedIn);

  if (!loggedIn) {
    renderRooms();
    showChatPlaceholder();
    return;
  }

  $("#user-name").textContent = state.user.display_name;
  $("#user-phone").textContent = state.user.phone;
  $("#avatar").textContent = state.user.display_name.slice(0, 1).toUpperCase();
}

function renderMode() {
  document.querySelectorAll(".tab").forEach((tab) => {
    const active = tab.dataset.mode === state.mode;
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-selected", String(active));
  });
  $("#display-name-row").classList.toggle("hidden", state.mode !== "register");
  $("#display-name").required = state.mode === "register";
  $("#auth-submit").textContent = state.mode === "register" ? "Create account" : "Login";
}

async function authenticate(event) {
  event.preventDefault();
  const phone = $("#phone").value.trim();
  const password = $("#password").value;
  const displayName = $("#display-name").value.trim();
  const endpoint = state.mode === "register" ? "/auth/register" : "/auth/login";
  const body = state.mode === "register" ? { phone, password, display_name: displayName } : { phone, password };

  try {
    $("#auth-submit").disabled = true;
    const payload = await apiRequest(endpoint, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    saveSession(payload);
    showToast(state.mode === "register" ? "Account created." : "Welcome back.");
  } catch (error) {
    showToast(error.message, "error");
  } finally {
    $("#auth-submit").disabled = false;
  }
}

async function loadRooms() {
  if (!state.token) return;

  try {
    state.rooms = await apiRequest("/rooms", {
      headers: authHeaders(),
    });
    renderRooms();
  } catch (error) {
    showToast(error.message, "error");
    if (error.message === "authentication failed") {
      clearSession();
    }
  }
}

async function createRoom(event) {
  event.preventDefault();
  const nameInput = $("#room-name");
  const name = nameInput.value.trim();
  if (!name) return;

  try {
    $("#create-room-button").disabled = true;
    const room = await apiRequest("/rooms", {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ name }),
    });
    nameInput.value = "";
    state.rooms = [room, ...state.rooms.filter((existing) => existing.id !== room.id)];
    renderRooms();
    connectRoom(room);
    showToast("Room created.");
  } catch (error) {
    showToast(error.message, "error");
  } finally {
    $("#create-room-button").disabled = false;
  }
}

function renderRooms() {
  const list = $("#room-list");
  list.replaceChildren();

  if (!state.rooms.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = state.token ? "No rooms yet. Create one to begin." : "Login to view rooms.";
    list.append(empty);
    return;
  }

  state.rooms.forEach((room) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `room-item ${state.activeRoom?.id === room.id ? "active" : ""}`.trim();
    button.dataset.roomId = room.id;

    const name = document.createElement("strong");
    name.textContent = room.name;
    const meta = document.createElement("span");
    meta.textContent = new Date(room.created_at).toLocaleString([], {
      month: "short",
      day: "numeric",
      hour: "2-digit",
      minute: "2-digit",
    });
    const code = document.createElement("span");
    code.textContent = `Code: ${room.id}`;
    button.append(name, meta, code);
    button.addEventListener("click", () => connectRoom(room));
    list.append(button);
  });
}

function showChatPlaceholder() {
  $("#chat-placeholder").classList.remove("hidden");
  $("#chat-area").classList.add("hidden");
  messages.replaceChildren();
  setStatus(socketStatus, "Disconnected");
  messageInput.disabled = true;
  sendButton.disabled = true;
}

function connectRoom(room) {
  if (!state.token) {
    showToast("Login before connecting to a room.", "error");
    return;
  }

  disconnectSocket();
  state.activeRoom = room;
  renderRooms();
  $("#chat-placeholder").classList.add("hidden");
  $("#chat-area").classList.remove("hidden");
  $("#room-title").textContent = room.name;
  messages.replaceChildren();
  setStatus(socketStatus, "Connecting", "connecting");

  const socket = new WebSocket(wsUrl(room.id, state.token));
  state.socket = socket;

  socket.addEventListener("open", () => {
    setStatus(socketStatus, "Connected", "online");
    messageInput.disabled = false;
    sendButton.disabled = false;
    messageInput.focus();
  });

  socket.addEventListener("message", (event) => {
    try {
      const payload = JSON.parse(event.data);
      if (payload.error) {
        showToast(payload.error, "error");
        return;
      }
      appendMessage(payload);
    } catch {
      showToast("Received an unreadable message.", "error");
    }
  });

  socket.addEventListener("close", () => {
    if (state.socket === socket) {
      state.socket = null;
      messageInput.disabled = true;
      sendButton.disabled = true;
      setStatus(socketStatus, "Disconnected");
    }
  });

  socket.addEventListener("error", () => {
    setStatus(socketStatus, "Error", "error");
    showToast("WebSocket connection failed.", "error");
  });
}

function disconnectSocket() {
  if (state.socket) {
    state.socket.close();
    state.socket = null;
  }
  messageInput.disabled = true;
  sendButton.disabled = true;
  if (socketStatus) setStatus(socketStatus, "Disconnected");
}

function appendMessage(event) {
  const mine = state.user && event.sender_id === state.user.id;
  const message = document.createElement("article");
  message.className = `message ${mine ? "mine" : ""}`.trim();

  const meta = document.createElement("div");
  meta.className = "message-meta";
  const sender = document.createElement("span");
  sender.textContent = mine ? "You" : event.sender_id.slice(0, 8);
  const time = document.createElement("time");
  time.dateTime = event.sent_at;
  time.textContent = new Date(event.sent_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  meta.append(sender, time);

  const body = document.createElement("div");
  body.className = "message-body";
  body.textContent = event.body;

  message.append(meta, body);
  messages.append(message);
  messages.scrollTop = messages.scrollHeight;
}

function sendMessage(event) {
  event.preventDefault();
  const body = messageInput.value.trim();
  if (!body || !state.socket || state.socket.readyState !== WebSocket.OPEN) return;
  state.socket.send(JSON.stringify({ body }));
  messageInput.value = "";
  messageInput.focus();
}

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    state.mode = tab.dataset.mode;
    renderMode();
  });
});

authForm.addEventListener("submit", authenticate);
roomForm.addEventListener("submit", createRoom);
messageForm.addEventListener("submit", sendMessage);
$("#refresh-rooms-button").addEventListener("click", loadRooms);
$("#logout-button").addEventListener("click", clearSession);

renderMode();
renderSession();
if (state.token && state.user) {
  loadRooms();
}
