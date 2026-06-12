const SESSION_TOKEN_KEY = "secure-chat-token";
const SESSION_USER_KEY = "secure-chat-user";
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const REACTION_EMOJIS = ["👍", "❤️", "😂", "😮", "😢", "🙏"];
const TYPING_SEND_INTERVAL_MS = 2000;
const TYPING_SHOW_MS = 3500;

const state = {
  mode: "login",
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) || "",
  user: readStoredUser(),
  friends: [], // FriendSummary objects: {user, online, unread_count, last_message}
  requests: [],
  activeFriend: null, // a friend's user object
  socket: null,
  replyTo: null, // message being replied to
  lastTypingSentAt: 0,
  typingUntil: 0,
  typingTimer: null,
};

// message id -> {message, element}; rebuilt per opened conversation.
const messageIndex = new Map();
const blobUrlCache = new Map();

const $ = (selector) => document.querySelector(selector);
const authForm = $("#auth-form");
const addFriendForm = $("#add-friend-form");
const messageForm = $("#message-form");
const messages = $("#messages");
const messageInput = $("#message-input");
const sendButton = $("#send-button");
const attachButton = $("#attach-button");
const imageInput = $("#image-input");
const socketStatus = $("#socket-status");
const presenceLine = $("#presence-line");
const replyBanner = $("#reply-banner");

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

function sendFrame(frame) {
  if (state.socket && state.socket.readyState === WebSocket.OPEN) {
    state.socket.send(JSON.stringify(frame));
    return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// Session
// ---------------------------------------------------------------------------

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
  state.replyTo = null;
  messageIndex.clear();
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

// ---------------------------------------------------------------------------
// Contacts: friends list (conversation list) and friend requests
// ---------------------------------------------------------------------------

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
    renderPresenceLine();
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

function friendSummary(friendId) {
  return state.friends.find((entry) => entry.user.id === friendId) || null;
}

function previewText(summary) {
  const last = summary.last_message;
  if (!last) return "No messages yet.";
  const prefix = last.sender_id === state.user.id ? "You: " : "";
  if (last.deleted) return `${prefix}Message deleted`;
  if (last.kind === "image") return `${prefix}📷 Photo`;
  return prefix + last.body;
}

function formatTime(iso) {
  return new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

function formatLastSeen(iso) {
  if (!iso) return "Offline";
  const date = new Date(iso);
  const today = new Date();
  const sameDay = date.toDateString() === today.toDateString();
  return `Last seen ${sameDay ? formatTime(iso) : date.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" })}`;
}

function sortFriends() {
  state.friends.sort((a, b) => {
    const aAt = a.last_message ? Date.parse(a.last_message.sent_at) : 0;
    const bAt = b.last_message ? Date.parse(b.last_message.sent_at) : 0;
    if (aAt !== bAt) return bAt - aAt;
    return a.user.display_name.localeCompare(b.user.display_name);
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

  sortFriends();
  state.friends.forEach((summary) => {
    const friend = summary.user;
    const button = document.createElement("button");
    button.type = "button";
    button.className = `friend-item ${state.activeFriend?.id === friend.id ? "active" : ""}`.trim();
    button.dataset.friendId = friend.id;

    const topRow = document.createElement("div");
    topRow.className = "friend-top";
    const name = document.createElement("strong");
    name.textContent = friend.display_name;
    const dot = document.createElement("span");
    dot.className = `presence-dot ${summary.online ? "online" : ""}`.trim();
    dot.title = summary.online ? "Online" : "Offline";
    name.prepend(dot);
    topRow.append(name);
    if (summary.last_message) {
      const time = document.createElement("span");
      time.className = "friend-time";
      time.textContent = formatTime(summary.last_message.sent_at);
      topRow.append(time);
    }

    const bottomRow = document.createElement("div");
    bottomRow.className = "friend-bottom";
    const preview = document.createElement("span");
    preview.className = "friend-preview";
    preview.textContent = previewText(summary);
    bottomRow.append(preview);
    if (summary.unread_count > 0) {
      const badge = document.createElement("span");
      badge.className = "unread-badge";
      badge.textContent = summary.unread_count > 99 ? "99+" : String(summary.unread_count);
      bottomRow.append(badge);
    }

    button.append(topRow, bottomRow);
    button.addEventListener("click", () => openConversation(friend));
    list.append(button);
  });
}

// ---------------------------------------------------------------------------
// Conversation
// ---------------------------------------------------------------------------

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
  state.replyTo = null;
  state.typingUntil = 0;
  renderReplyBanner();
  renderFriends();
  $("#chat-placeholder").classList.add("hidden");
  $("#chat-area").classList.remove("hidden");
  $("#chat-title").textContent = `${friend.display_name} (${friend.phone})`;
  messages.replaceChildren();
  messageIndex.clear();
  renderPresenceLine();

  try {
    const history = await apiRequest(`/messages/${friend.id}`, { headers: authHeaders() });
    messages.replaceChildren();
    messageIndex.clear();
    history.forEach(appendMessage);
    markConversationRead(friend.id);
  } catch (error) {
    showToast(error.message, "error");
  }

  updateComposerState();
  if (!messageInput.disabled) messageInput.focus();
  if (!state.socket || state.socket.readyState === WebSocket.CLOSED) {
    connectSocket();
  }
}

function markConversationRead(peerId) {
  sendFrame({ type: "read", peer_id: peerId });
  const summary = friendSummary(peerId);
  if (summary && summary.unread_count) {
    summary.unread_count = 0;
    renderFriends();
  }
}

function renderPresenceLine() {
  if (!state.activeFriend) return;
  const summary = friendSummary(state.activeFriend.id);
  if (!summary) {
    presenceLine.textContent = "";
    return;
  }
  if (Date.now() < state.typingUntil) {
    presenceLine.textContent = "typing…";
    presenceLine.className = "presence-line typing";
  } else if (summary.online) {
    presenceLine.textContent = "Online";
    presenceLine.className = "presence-line online";
  } else {
    presenceLine.textContent = formatLastSeen(summary.user.last_seen_at);
    presenceLine.className = "presence-line";
  }
}

function updateComposerState() {
  const connected = state.socket && state.socket.readyState === WebSocket.OPEN;
  const ready = Boolean(connected && state.activeFriend);
  messageInput.disabled = !ready;
  sendButton.disabled = !ready;
  attachButton.disabled = !ready;
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

function connectSocket() {
  if (!state.token) return;
  if (state.socket && state.socket.readyState !== WebSocket.CLOSED) return;

  setStatus(socketStatus, "Connecting", "connecting");
  const socket = new WebSocket(wsUrl(state.token));
  state.socket = socket;

  socket.addEventListener("open", () => {
    setStatus(socketStatus, "Connected", "online");
    updateComposerState();
    if (state.activeFriend) markConversationRead(state.activeFriend.id);
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
  attachButton.disabled = true;
  if (socketStatus) setStatus(socketStatus, "Disconnected");
}

function handleSocketEvent(event) {
  switch (event.type) {
    case "error":
      showToast(event.error, "error");
      return;
    case "message":
      handleIncomingMessage(event.message);
      return;
    case "message_deleted":
      handleMessageDeleted(event);
      return;
    case "delivered":
      updateReceipts(event.message_ids, "delivered");
      return;
    case "read":
      updateReceipts(event.message_ids, "read");
      return;
    case "typing":
      handleTypingEvent(event.from);
      return;
    case "presence":
      handlePresenceEvent(event);
      return;
    case "reaction":
      handleReactionEvent(event);
      return;
    default:
  }
}

function handleIncomingMessage(message) {
  const mine = message.sender_id === state.user.id;
  const peerId = mine ? message.recipient_id : message.sender_id;
  const summary = friendSummary(peerId);
  if (summary) {
    summary.last_message = message;
  }

  if (state.activeFriend && peerId === state.activeFriend.id) {
    appendMessage(message);
    if (!mine) {
      // Conversation is open: mark read right away.
      sendFrame({ type: "read", peer_id: peerId });
    }
  } else if (!mine) {
    // Background conversation: ack delivery and bump the unread badge.
    sendFrame({ type: "delivered", message_ids: [message.id] });
    if (summary) summary.unread_count += 1;
    const sender = summary?.user.display_name || "a friend";
    showToast(`New message from ${sender}.`);
  }
  renderFriends();
}

function handleMessageDeleted(event) {
  const entry = messageIndex.get(event.message_id);
  if (entry) {
    entry.message.deleted = true;
    entry.message.body = "";
    entry.message.attachment_id = null;
    entry.message.reactions = [];
    rerenderMessage(entry);
  }
  const peerId = event.sender_id === state.user.id ? event.recipient_id : event.sender_id;
  const summary = friendSummary(peerId);
  if (summary?.last_message?.id === event.message_id) {
    summary.last_message.deleted = true;
    summary.last_message.body = "";
    renderFriends();
  }
}

function updateReceipts(messageIds, level) {
  const now = new Date().toISOString();
  messageIds.forEach((id) => {
    const entry = messageIndex.get(id);
    if (!entry) return;
    if (level === "delivered") entry.message.delivered_at ||= now;
    if (level === "read") {
      entry.message.delivered_at ||= now;
      entry.message.read_at ||= now;
    }
    renderTicks(entry);
  });
}

function handleTypingEvent(fromUserId) {
  if (!state.activeFriend || fromUserId !== state.activeFriend.id) return;
  state.typingUntil = Date.now() + TYPING_SHOW_MS;
  renderPresenceLine();
  window.clearTimeout(state.typingTimer);
  state.typingTimer = window.setTimeout(renderPresenceLine, TYPING_SHOW_MS + 50);
}

function handlePresenceEvent(event) {
  const summary = friendSummary(event.user_id);
  if (!summary) return;
  summary.online = event.online;
  if (event.last_seen_at) summary.user.last_seen_at = event.last_seen_at;
  renderFriends();
  renderPresenceLine();
}

function handleReactionEvent(event) {
  const entry = messageIndex.get(event.message_id);
  if (!entry) return;
  const reactions = entry.message.reactions;
  let group = reactions.find((reaction) => reaction.emoji === event.emoji);
  if (event.added) {
    if (!group) {
      group = { emoji: event.emoji, user_ids: [] };
      reactions.push(group);
    }
    if (!group.user_ids.includes(event.user_id)) group.user_ids.push(event.user_id);
  } else if (group) {
    group.user_ids = group.user_ids.filter((id) => id !== event.user_id);
    if (!group.user_ids.length) {
      entry.message.reactions = reactions.filter((reaction) => reaction !== group);
    }
  }
  renderReactions(entry);
}

// ---------------------------------------------------------------------------
// Message rendering
// ---------------------------------------------------------------------------

async function attachmentUrl(attachmentId) {
  if (blobUrlCache.has(attachmentId)) return blobUrlCache.get(attachmentId);
  const response = await fetch(apiUrl(`/attachments/${attachmentId}`), { headers: authHeaders() });
  if (!response.ok) throw new Error("Could not load image.");
  const url = URL.createObjectURL(await response.blob());
  blobUrlCache.set(attachmentId, url);
  return url;
}

function renderImageBody(body, attachmentId) {
  const placeholder = document.createElement("span");
  placeholder.className = "image-loading";
  placeholder.textContent = "Loading image…";
  body.append(placeholder);

  attachmentUrl(attachmentId)
    .then((url) => {
      const image = document.createElement("img");
      image.className = "message-image";
      image.alt = "Sent image";
      image.src = url;
      placeholder.replaceWith(image);
    })
    .catch(() => {
      placeholder.textContent = "Image unavailable.";
    });
}

function senderName(senderId) {
  if (senderId === state.user.id) return "You";
  return state.activeFriend?.display_name || "Friend";
}

function renderTicks(entry) {
  const ticks = entry.element.querySelector(".ticks");
  if (!ticks) return;
  const message = entry.message;
  if (message.read_at) {
    ticks.textContent = "✓✓";
    ticks.className = "ticks read";
    ticks.title = "Read";
  } else if (message.delivered_at) {
    ticks.textContent = "✓✓";
    ticks.className = "ticks";
    ticks.title = "Delivered";
  } else {
    ticks.textContent = "✓";
    ticks.className = "ticks";
    ticks.title = "Sent";
  }
}

function renderReactions(entry) {
  const container = entry.element.querySelector(".reaction-chips");
  if (!container) return;
  container.replaceChildren();
  entry.message.reactions.forEach((reaction) => {
    const chip = document.createElement("button");
    chip.type = "button";
    const mine = reaction.user_ids.includes(state.user.id);
    chip.className = `reaction-chip ${mine ? "mine" : ""}`.trim();
    chip.textContent = reaction.user_ids.length > 1 ? `${reaction.emoji} ${reaction.user_ids.length}` : reaction.emoji;
    chip.title = mine ? "Tap to remove your reaction" : "Tap to react too";
    chip.addEventListener("click", () => {
      sendFrame({ type: "reaction", message_id: entry.message.id, emoji: reaction.emoji });
    });
    container.append(chip);
  });
}

function buildMessageElement(message) {
  const mine = message.sender_id === state.user.id;
  const element = document.createElement("article");
  element.className = `message ${mine ? "mine" : ""}`.trim();
  element.dataset.messageId = message.id;

  const meta = document.createElement("div");
  meta.className = "message-meta";
  const sender = document.createElement("span");
  sender.textContent = senderName(message.sender_id);
  const time = document.createElement("time");
  time.dateTime = message.sent_at;
  time.textContent = formatTime(message.sent_at);
  meta.append(sender, time);
  if (mine && !message.deleted) {
    const ticks = document.createElement("span");
    ticks.className = "ticks";
    meta.append(ticks);
  }

  if (message.reply_to) {
    const quote = document.createElement("div");
    quote.className = "reply-quote";
    const quoteName = document.createElement("strong");
    quoteName.textContent = senderName(message.reply_to.sender_id);
    const quoteBody = document.createElement("span");
    if (message.reply_to.deleted) {
      quoteBody.textContent = "Message deleted";
      quote.classList.add("deleted");
    } else if (message.reply_to.kind === "image") {
      quoteBody.textContent = "📷 Photo";
    } else {
      quoteBody.textContent = message.reply_to.body;
    }
    quote.append(quoteName, quoteBody);
    element.append(quote);
  }

  const body = document.createElement("div");
  body.className = "message-body";
  if (message.deleted) {
    element.classList.add("deleted");
    body.textContent = "This message was deleted.";
  } else if (message.kind === "image" && message.attachment_id) {
    renderImageBody(body, message.attachment_id);
  } else {
    body.textContent = message.body;
  }

  element.prepend(meta);
  element.append(body);

  const chips = document.createElement("div");
  chips.className = "reaction-chips";
  element.append(chips);

  if (!message.deleted) {
    element.append(buildMessageActions(message, element));
  }
  return element;
}

function buildMessageActions(message, element) {
  const mine = message.sender_id === state.user.id;
  const actions = document.createElement("div");
  actions.className = "message-actions";

  const reply = document.createElement("button");
  reply.type = "button";
  reply.className = "message-action";
  reply.textContent = "Reply";
  reply.addEventListener("click", () => {
    state.replyTo = message;
    renderReplyBanner();
    messageInput.focus();
  });
  actions.append(reply);

  const react = document.createElement("button");
  react.type = "button";
  react.className = "message-action";
  react.textContent = "React";
  react.addEventListener("click", (clickEvent) => {
    clickEvent.stopPropagation();
    toggleReactionPicker(actions, message);
  });
  actions.append(react);

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
  return actions;
}

function toggleReactionPicker(anchor, message) {
  const existing = anchor.querySelector(".reaction-picker");
  if (existing) {
    existing.remove();
    return;
  }
  document.querySelectorAll(".reaction-picker").forEach((picker) => picker.remove());
  const picker = document.createElement("div");
  picker.className = "reaction-picker";
  REACTION_EMOJIS.forEach((emoji) => {
    const option = document.createElement("button");
    option.type = "button";
    option.textContent = emoji;
    option.addEventListener("click", () => {
      sendFrame({ type: "reaction", message_id: message.id, emoji });
      picker.remove();
    });
    picker.append(option);
  });
  anchor.append(picker);
}

function rerenderMessage(entry) {
  const replacement = buildMessageElement(entry.message);
  entry.element.replaceWith(replacement);
  entry.element = replacement;
  renderTicks(entry);
  renderReactions(entry);
}

function appendMessage(message) {
  const element = buildMessageElement(message);
  const entry = { message, element };
  messageIndex.set(message.id, entry);
  messages.append(element);
  renderTicks(entry);
  renderReactions(entry);
  messages.scrollTop = messages.scrollHeight;
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
      messageIndex.delete(message.id);
      element.remove();
    }
    // scope=everyone is reflected via the message_deleted WebSocket event.
  } catch (error) {
    showToast(error.message, "error");
  }
}

// ---------------------------------------------------------------------------
// Composer: text, images, replies, typing
// ---------------------------------------------------------------------------

function renderReplyBanner() {
  if (!state.replyTo) {
    replyBanner.classList.add("hidden");
    return;
  }
  replyBanner.classList.remove("hidden");
  $("#reply-banner-name").textContent = `Replying to ${senderName(state.replyTo.sender_id)}`;
  $("#reply-banner-body").textContent =
    state.replyTo.kind === "image" ? "📷 Photo" : state.replyTo.body;
}

function sendMessage(event) {
  event.preventDefault();
  const body = messageInput.value.trim();
  if (!body || !state.activeFriend) return;
  const frame = { type: "message", to: state.activeFriend.id, body };
  if (state.replyTo) frame.reply_to = state.replyTo.id;
  if (!sendFrame(frame)) return;
  state.replyTo = null;
  renderReplyBanner();
  messageInput.value = "";
  messageInput.focus();
}

async function sendImage(file) {
  if (!file || !state.activeFriend) return;
  if (file.size > MAX_IMAGE_BYTES) {
    showToast("Images must be 5 MB or smaller.", "error");
    return;
  }

  try {
    attachButton.disabled = true;
    const upload = await apiRequest("/attachments", {
      method: "POST",
      headers: authHeaders({ "content-type": file.type || "application/octet-stream" }),
      body: file,
    });
    const frame = { type: "message", to: state.activeFriend.id, attachment_id: upload.id };
    if (state.replyTo) frame.reply_to = state.replyTo.id;
    sendFrame(frame);
    state.replyTo = null;
    renderReplyBanner();
  } catch (error) {
    const friendly =
      error.message === "invalid request"
        ? "Only PNG, JPEG, GIF, or WebP images are supported."
        : error.message;
    showToast(friendly, "error");
  } finally {
    attachButton.disabled = false;
    updateComposerState();
  }
}

function notifyTyping() {
  if (!state.activeFriend) return;
  const now = Date.now();
  if (now - state.lastTypingSentAt < TYPING_SEND_INTERVAL_MS) return;
  if (sendFrame({ type: "typing", to: state.activeFriend.id })) {
    state.lastTypingSentAt = now;
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

document.querySelectorAll(".tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    state.mode = tab.dataset.mode;
    renderMode();
  });
});

authForm.addEventListener("submit", authenticate);
addFriendForm.addEventListener("submit", sendFriendRequest);
messageForm.addEventListener("submit", sendMessage);
messageInput.addEventListener("input", notifyTyping);
attachButton.addEventListener("click", () => imageInput.click());
imageInput.addEventListener("change", () => {
  const [file] = imageInput.files;
  imageInput.value = "";
  sendImage(file);
});
$("#cancel-reply").addEventListener("click", () => {
  state.replyTo = null;
  renderReplyBanner();
});
document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && state.replyTo) {
    state.replyTo = null;
    renderReplyBanner();
  }
});
document.addEventListener("click", (event) => {
  if (!event.target.closest(".reaction-picker") && !event.target.closest(".message-action")) {
    document.querySelectorAll(".reaction-picker").forEach((picker) => picker.remove());
  }
});
$("#refresh-button").addEventListener("click", loadContacts);
$("#logout-button").addEventListener("click", clearSession);

renderMode();
renderSession();
if (state.token && state.user) {
  connectSocket();
  loadContacts();
}
