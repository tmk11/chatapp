const SESSION_TOKEN_KEY = "secure-chat-token";
const SESSION_USER_KEY = "secure-chat-user";

const state = {
  mode: "login",
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) || "",
  user: readStoredUser(),
  friends: [],
  requests: [],
  activeFriend: null,
  socket: null,
};

const $ = (selector) => document.querySelector(selector);
const authForm = $("#auth-form");
const addFriendForm = $("#add-friend-form");
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

function wsUrl(token) {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  const params = new URLSearchParams({ token });
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
  connectSocket();
  loadContacts();
}

function clearSession() {
  disconnectSocket();
  state.token = "";
  state.user = null;
  state.friends = [];
  state.requests = [];
  state.activeFriend = null;
  sessionStorage.removeItem(SESSION_TOKEN_KEY);
  sessionStorage.removeItem(SESSION_USER_KEY);
  renderSession();
}

function renderSession() {
  const loggedIn = Boolean(state.token && state.user);
  $("#auth-screen").classList.toggle("hidden", loggedIn);
  $("#app-screen").classList.toggle("hidden", !loggedIn);

  if (!loggedIn) {
    renderFriends();
    renderRequests();
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

async function loadContacts() {
  if (!state.token) return;

  try {
    const [friends, requests] = await Promise.all([
      apiRequest("/friends", { headers: authHeaders() }),
      apiRequest("/friends/requests", { headers: authHeaders() }),
    ]);
    state.friends = friends;
    state.requests = requests;
    renderFriends();
    renderRequests();
  } catch (error) {
    showToast(error.message, "error");
    if (error.message === "authentication failed") {
      clearSession();
    }
  }
}

async function sendFriendRequest(event) {
  event.preventDefault();
  const phoneInput = $("#friend-phone");
  const phone = phoneInput.value.trim();
  if (!phone) return;

  try {
    $("#add-friend-button").disabled = true;
    const result = await apiRequest("/friends/requests", {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ phone }),
    });
    phoneInput.value = "";
    if (result.status === "accepted") {
      showToast("You are now friends.");
      await loadContacts();
    } else {
      showToast("Friend request sent.");
    }
  } catch (error) {
    const friendly =
      error.message === "not found"
        ? "No account found with that phone number."
        : error.message === "resource conflict"
          ? "You are already friends."
          : error.message;
    showToast(friendly, "error");
  } finally {
    $("#add-friend-button").disabled = false;
  }
}

async function respondToRequest(requestId, accept) {
  try {
    await apiRequest(`/friends/requests/${requestId}`, {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ accept }),
    });
    showToast(accept ? "Friend request accepted." : "Friend request declined.");
    await loadContacts();
  } catch (error) {
    showToast(error.message, "error");
  }
}

function renderRequests() {
  const section = $("#requests-section");
  const list = $("#request-list");
  list.replaceChildren();
  section.classList.toggle("hidden", !state.requests.length);

  state.requests.forEach((request) => {
    const item = document.createElement("div");
    item.className = "request-item";

    const info = document.createElement("div");
    const name = document.createElement("strong");
    name.textContent = request.from.display_name;
    const phone = document.createElement("span");
    phone.textContent = request.from.phone;
    info.append(name, phone);

    const actions = document.createElement("div");
    actions.className = "request-actions";
    const accept = document.createElement("button");
    accept.type = "button";
    accept.className = "secondary-button small";
    accept.textContent = "Accept";
    accept.addEventListener("click", () => respondToRequest(request.id, true));
    const decline = document.createElement("button");
    decline.type = "button";
    decline.className = "ghost-button small";
    decline.textContent = "Decline";
    decline.addEventListener("click", () => respondToRequest(request.id, false));
    actions.append(accept, decline);

    item.append(info, actions);
    list.append(item);
  });
}

function renderFriends() {
  const list = $("#friend-list");
  list.replaceChildren();

  if (!state.friends.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = state.token ? "No friends yet. Send a request to begin." : "Login to view friends.";
    list.append(empty);
    return;
  }

  state.friends.forEach((friend) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = `friend-item ${state.activeFriend?.id === friend.id ? "active" : ""}`.trim();
    button.dataset.friendId = friend.id;

    const name = document.createElement("strong");
    name.textContent = friend.display_name;
    const phone = document.createElement("span");
    phone.textContent = friend.phone;
    button.append(name, phone);
    button.addEventListener("click", () => openConversation(friend));
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

async function openConversation(friend) {
  state.activeFriend = friend;
  renderFriends();
  $("#chat-placeholder").classList.add("hidden");
  $("#chat-area").classList.remove("hidden");
  $("#chat-title").textContent = `${friend.display_name} (${friend.phone})`;
  messages.replaceChildren();

  try {
    const history = await apiRequest(`/messages/${friend.id}`, { headers: authHeaders() });
    messages.replaceChildren();
    history.forEach(appendMessage);
  } catch (error) {
    showToast(error.message, "error");
  }

  updateComposerState();
  if (!messageInput.disabled) messageInput.focus();
  if (!state.socket || state.socket.readyState === WebSocket.CLOSED) {
    connectSocket();
  }
}

function updateComposerState() {
  const connected = state.socket && state.socket.readyState === WebSocket.OPEN;
  const ready = Boolean(connected && state.activeFriend);
  messageInput.disabled = !ready;
  sendButton.disabled = !ready;
}

function connectSocket() {
  if (!state.token) return;
  if (state.socket && state.socket.readyState !== WebSocket.CLOSED) return;

  setStatus(socketStatus, "Connecting", "connecting");
  const socket = new WebSocket(wsUrl(state.token));
  state.socket = socket;

  socket.addEventListener("open", () => {
    setStatus(socketStatus, "Connected", "online");
    updateComposerState();
  });

  socket.addEventListener("message", (event) => {
    try {
      handleSocketEvent(JSON.parse(event.data));
    } catch {
      showToast("Received an unreadable message.", "error");
    }
  });

  socket.addEventListener("close", () => {
    if (state.socket === socket) {
      state.socket = null;
      setStatus(socketStatus, "Disconnected");
      updateComposerState();
    }
  });

  socket.addEventListener("error", () => {
    setStatus(socketStatus, "Error", "error");
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

function handleSocketEvent(event) {
  if (event.type === "error") {
    showToast(event.error, "error");
    return;
  }

  if (event.type === "message") {
    const message = event.message;
    const peerId = message.sender_id === state.user.id ? message.recipient_id : message.sender_id;
    if (state.activeFriend && peerId === state.activeFriend.id) {
      appendMessage(message);
    } else {
      const sender = state.friends.find((friend) => friend.id === message.sender_id);
      showToast(`New message from ${sender ? sender.display_name : "a friend"}.`);
    }
    return;
  }

  if (event.type === "message_deleted") {
    const element = messages.querySelector(`[data-message-id="${event.message_id}"]`);
    if (element) markDeleted(element);
  }
}

function markDeleted(element) {
  element.classList.add("deleted");
  const body = element.querySelector(".message-body");
  body.textContent = "This message was deleted.";
  const actions = element.querySelector(".message-actions");
  if (actions) actions.remove();
}

async function deleteMessage(message, element, scope) {
  const confirmText =
    scope === "everyone"
      ? "Delete this message for everyone? This cannot be undone."
      : "Delete this message for you only?";
  if (!window.confirm(confirmText)) return;

  try {
    await apiRequest(`/messages/${message.id}?scope=${scope}`, {
      method: "DELETE",
      headers: authHeaders(),
    });
    if (scope === "me") {
      element.remove();
    } else {
      markDeleted(element);
    }
  } catch (error) {
    showToast(error.message, "error");
  }
}

function appendMessage(message) {
  const mine = state.user && message.sender_id === state.user.id;
  const element = document.createElement("article");
  element.className = `message ${mine ? "mine" : ""}`.trim();
  element.dataset.messageId = message.id;

  const meta = document.createElement("div");
  meta.className = "message-meta";
  const sender = document.createElement("span");
  sender.textContent = mine ? "You" : state.activeFriend?.display_name || "Friend";
  const time = document.createElement("time");
  time.dateTime = message.sent_at;
  time.textContent = new Date(message.sent_at).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  meta.append(sender, time);

  const body = document.createElement("div");
  body.className = "message-body";
  body.textContent = message.body;

  element.append(meta, body);

  if (message.deleted) {
    element.classList.add("deleted");
    body.textContent = "This message was deleted.";
  } else {
    const actions = document.createElement("div");
    actions.className = "message-actions";
    const deleteForMe = document.createElement("button");
    deleteForMe.type = "button";
    deleteForMe.className = "message-action";
    deleteForMe.textContent = "Delete for me";
    deleteForMe.addEventListener("click", () => deleteMessage(message, element, "me"));
    actions.append(deleteForMe);

    if (mine) {
      const deleteForEveryone = document.createElement("button");
      deleteForEveryone.type = "button";
      deleteForEveryone.className = "message-action danger";
      deleteForEveryone.textContent = "Delete for everyone";
      deleteForEveryone.addEventListener("click", () => deleteMessage(message, element, "everyone"));
      actions.append(deleteForEveryone);
    }
    element.append(actions);
  }

  messages.append(element);
  messages.scrollTop = messages.scrollHeight;
}

function sendMessage(event) {
  event.preventDefault();
  const body = messageInput.value.trim();
  if (!body || !state.activeFriend || !state.socket || state.socket.readyState !== WebSocket.OPEN) return;
  state.socket.send(JSON.stringify({ to: state.activeFriend.id, body }));
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
addFriendForm.addEventListener("submit", sendFriendRequest);
messageForm.addEventListener("submit", sendMessage);
$("#refresh-button").addEventListener("click", loadContacts);
$("#logout-button").addEventListener("click", clearSession);

renderMode();
renderSession();
if (state.token && state.user) {
  connectSocket();
  loadContacts();
}
