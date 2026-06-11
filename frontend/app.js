const SESSION_TOKEN_KEY = "secure-chat-token";
const SESSION_USER_KEY = "secure-chat-user";

const state = {
  mode: "login",
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) || "",
  user: readStoredUser(),
  socket: null,
  roomId: "demo",
};

const $ = (selector) => document.querySelector(selector);
const authForm = $("#auth-form");
const messageForm = $("#message-form");
const roomForm = $("#room-form");
const messages = $("#messages");
const messageInput = $("#message-input");
const sendButton = $("#send-button");
const socketStatus = $("#socket-status");
const authStatus = $("#auth-status");

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

function setStatus(element, label, variant = "") {
  element.textContent = label;
  element.className = `status-pill ${variant}`.trim();
}

function showToast(message, type = "success") {
  const toast = document.createElement("div");
  toast.className = `toast ${type}`;
  toast.textContent = message;
  $("#toast-region").append(toast);
  window.setTimeout(() => toast.remove(), 4200);
}

function saveSession(payload) {
  state.token = payload.token;
  state.user = payload.user;
  sessionStorage.setItem(SESSION_TOKEN_KEY, payload.token);
  sessionStorage.setItem(SESSION_USER_KEY, JSON.stringify(payload.user));
  renderUser();
}

function clearSession() {
  disconnectSocket();
  state.token = "";
  state.user = null;
  sessionStorage.removeItem(SESSION_TOKEN_KEY);
  sessionStorage.removeItem(SESSION_USER_KEY);
  renderUser();
}

function renderUser() {
  const loggedIn = Boolean(state.token && state.user);
  $("#user-card").classList.toggle("hidden", !loggedIn);
  setStatus(authStatus, loggedIn ? "Online" : "Offline", loggedIn ? "online" : "");

  if (loggedIn) {
    $("#user-name").textContent = state.user.display_name;
    $("#user-phone").textContent = state.user.phone;
    $("#avatar").textContent = state.user.display_name.slice(0, 1).toUpperCase();
  }
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
    const response = await fetch(apiUrl(endpoint), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
    });
    const payload = await response.json().catch(() => ({}));
    if (!response.ok) {
      throw new Error(payload.error || "Authentication failed");
    }
    saveSession(payload);
    showToast(state.mode === "register" ? "Account created." : "Welcome back.");
  } catch (error) {
    showToast(error.message, "error");
  } finally {
    $("#auth-submit").disabled = false;
  }
}

function connectSocket(roomId) {
  if (!state.token) {
    showToast("Login before connecting to a room.", "error");
    return;
  }
  const nextRoomId = roomId.trim();
  if (!nextRoomId) {
    showToast("Enter a room ID before connecting.", "error");
    return;
  }

  disconnectSocket();
  state.roomId = nextRoomId;
  $("#room-title").textContent = state.roomId;
  setStatus(socketStatus, "Connecting", "connecting");

  const socket = new WebSocket(wsUrl(state.roomId, state.token));
  state.socket = socket;

  socket.addEventListener("open", () => {
    setStatus(socketStatus, "Connected", "online");
    messageInput.disabled = false;
    sendButton.disabled = false;
    showToast(`Connected to #${state.roomId}.`);
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
  setStatus(socketStatus, "Disconnected");
}

function appendMessage(event) {
  const emptyState = messages.querySelector(".empty-state");
  if (emptyState) emptyState.remove();

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
messageForm.addEventListener("submit", sendMessage);
roomForm.addEventListener("submit", (event) => {
  event.preventDefault();
  connectSocket($("#room-id").value);
});
$("#logout-button").addEventListener("click", clearSession);

renderMode();
renderUser();
