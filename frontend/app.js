const SESSION_TOKEN_KEY = "secure-chat-token";
const SESSION_USER_KEY = "secure-chat-user";
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const MAX_VOICE_MS = 10 * 60 * 1000;
const REACTION_EMOJIS = ["👍", "❤️", "😂", "😮", "😢", "🙏"];
const TYPING_SEND_INTERVAL_MS = 2000;
const TYPING_SHOW_MS = 3500;

const AVATAR_GRADIENTS = [
  "linear-gradient(135deg, #6d5efc, #a855f7)",
  "linear-gradient(135deg, #ff5c8a, #ff8f6b)",
  "linear-gradient(135deg, #22c55e, #14b8a6)",
  "linear-gradient(135deg, #38bdf8, #6366f1)",
  "linear-gradient(135deg, #f59e0b, #ef4444)",
  "linear-gradient(135deg, #ec4899, #8b5cf6)",
  "linear-gradient(135deg, #06b6d4, #3b82f6)",
  "linear-gradient(135deg, #a855f7, #ec4899)",
];

const ICONS = {
  reply: '<path d="M9 14 4 9l5-5"/><path d="M4 9h11a5 5 0 0 1 5 5v2"/>',
  react:
    '<circle cx="12" cy="12" r="9"/><path d="M8 14s1.5 2 4 2 4-2 4-2"/><line x1="9" y1="9" x2="9.01" y2="9"/><line x1="15" y1="9" x2="15.01" y2="9"/>',
  pin: '<path d="M12 17v5M9 10.76V6a3 3 0 0 1 6 0v4.76a2 2 0 0 0 .59 1.42L18 14H6l2.41-1.82A2 2 0 0 0 9 10.76Z"/>',
  hide: '<path d="M3 3l18 18"/><path d="M10.6 10.6a2 2 0 0 0 2.8 2.8"/><path d="M9.4 5.2A9.7 9.7 0 0 1 12 5c6 0 9 7 9 7a16 16 0 0 1-2.3 3.3M6.2 6.2A16 16 0 0 0 3 12s3 7 9 7a9 9 0 0 0 3.2-.6"/>',
  trash:
    '<path d="M3 6h18M8 6V4a2 2 0 0 1 2-2h4a2 2 0 0 1 2 2v2m2 0v14a2 2 0 0 1-2 2H7a2 2 0 0 1-2-2V6"/><line x1="10" y1="11" x2="10" y2="17"/><line x1="14" y1="11" x2="14" y2="17"/>',
  check: '<path d="M20 6 9 17l-5-5"/>',
  close: '<path d="M18 6 6 18M6 6l12 12"/>',
};

const state = {
  mode: "login",
  token: sessionStorage.getItem(SESSION_TOKEN_KEY) || "",
  user: readStoredUser(),
  conversations: [],
  friends: [],
  requests: [],
  active: null, // active ConversationDto
  socket: null,
  replyTo: null,
  lastTypingSentAt: 0,
  typingUntil: 0,
  typingTimer: null,
  modalTab: "direct",
  recorder: null,
  recordingChunks: [],
  recordingStart: 0,
  recordingTimer: null,
};

const messageIndex = new Map(); // message id -> { message, element }
const blobUrlCache = new Map();

const $ = (s) => document.querySelector(s);
const appScreen = $("#app-screen");
const messages = $("#messages");
const messageInput = $("#message-input");
const sendButton = $("#send-button");
const attachButton = $("#attach-button");
const micButton = $("#mic-button");
const imageInput = $("#image-input");
const socketStatus = $("#socket-status");
const presenceLine = $("#presence-line");
const replyBanner = $("#reply-banner");
const chatAvatar = $("#chat-avatar");
const pinnedBar = $("#pinned-bar");
const searchBar = $("#search-bar");
const searchInput = $("#search-input");
const modalBackdrop = $("#modal-backdrop");
const pickerList = $("#picker-list");
const avatarInput = (() => {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = "image/png,image/jpeg,image/gif,image/webp";
  input.className = "hidden";
  document.body.append(input);
  return input;
})();

function readStoredUser() {
  const raw = sessionStorage.getItem(SESSION_USER_KEY);
  if (!raw) return null;
  try {
    return JSON.parse(raw);
  } catch {
    sessionStorage.removeItem(SESSION_USER_KEY);
    return null;
  }
}

const apiUrl = (path) => `${window.location.origin}${path}`;
function wsUrl(token) {
  const protocol = window.location.protocol === "https:" ? "wss:" : "ws:";
  return `${protocol}//${window.location.host}/ws?${new URLSearchParams({ token })}`;
}
const authHeaders = (extra = {}) => ({ ...extra, Authorization: `Bearer ${state.token}` });

async function apiRequest(path, options = {}) {
  const response = await fetch(apiUrl(path), options);
  if (response.status === 204) return null;
  const payload = await response.json().catch(() => null);
  if (!response.ok) throw new Error(payload?.error || "Request failed");
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

function translateError(message) {
  const map = {
    "authentication failed": "Số điện thoại hoặc mật khẩu không đúng.",
    "not found": "Không tìm thấy.",
    "resource conflict": "Đã tồn tại.",
    "invalid request": "Yêu cầu không hợp lệ.",
    forbidden: "Bạn không có quyền.",
  };
  return map[message] || message;
}

function gradientFor(key) {
  const text = String(key || "?");
  let hash = 0;
  for (let i = 0; i < text.length; i += 1) hash = (hash + text.charCodeAt(i)) % AVATAR_GRADIENTS.length;
  return AVATAR_GRADIENTS[hash];
}

/** info: {id, display_name/title, avatar_attachment_id} */
function applyAvatar(element, info) {
  const name = info.display_name || info.title || "?";
  element.style.backgroundImage = "";
  element.style.background = gradientFor(info.id || name);
  element.textContent = name.trim().slice(0, 1).toUpperCase() || "?";
  if (info.avatar_attachment_id) {
    attachmentUrl(info.avatar_attachment_id)
      .then((url) => {
        element.style.backgroundImage = `url("${url}")`;
        element.style.backgroundSize = "cover";
        element.style.backgroundPosition = "center";
        element.textContent = "";
      })
      .catch(() => {});
  }
}

function iconButton(className, title, svg, onClick) {
  const button = document.createElement("button");
  button.type = "button";
  button.className = className;
  button.title = title;
  button.setAttribute("aria-label", title);
  button.innerHTML = `<svg viewBox="0 0 24 24" aria-hidden="true">${svg}</svg>`;
  if (onClick) button.addEventListener("click", onClick);
  return button;
}

async function attachmentUrl(attachmentId) {
  if (blobUrlCache.has(attachmentId)) return blobUrlCache.get(attachmentId);
  const response = await fetch(apiUrl(`/attachments/${attachmentId}`), { headers: authHeaders() });
  if (!response.ok) throw new Error("Could not load attachment.");
  const url = URL.createObjectURL(await response.blob());
  blobUrlCache.set(attachmentId, url);
  return url;
}

// ---------------------------------------------------------------------------
// Session & auth
// ---------------------------------------------------------------------------

function saveSession(payload) {
  state.token = payload.token;
  state.user = payload.user;
  sessionStorage.setItem(SESSION_TOKEN_KEY, payload.token);
  sessionStorage.setItem(SESSION_USER_KEY, JSON.stringify(payload.user));
  renderSession();
  connectSocket();
  loadAll();
}

function clearSession() {
  disconnectSocket();
  Object.assign(state, { token: "", user: null, conversations: [], friends: [], requests: [], active: null, replyTo: null });
  messageIndex.clear();
  appScreen.classList.remove("show-chat");
  sessionStorage.removeItem(SESSION_TOKEN_KEY);
  sessionStorage.removeItem(SESSION_USER_KEY);
  renderSession();
}

function renderSession() {
  const loggedIn = Boolean(state.token && state.user);
  $("#auth-screen").classList.toggle("hidden", loggedIn);
  appScreen.classList.toggle("hidden", !loggedIn);
  if (!loggedIn) {
    renderConversations();
    renderRequests();
    showChatPlaceholder();
    return;
  }
  $("#user-name").textContent = state.user.display_name;
  $("#user-phone").textContent = state.user.phone;
  applyAvatar($("#avatar"), state.user);
}

function renderMode() {
  document.querySelectorAll(".tabs:not(.modal-tabs) .tab").forEach((tab) => {
    const active = tab.dataset.mode === state.mode;
    tab.classList.toggle("active", active);
    tab.setAttribute("aria-selected", String(active));
  });
  $("#display-name-row").classList.toggle("hidden", state.mode !== "register");
  $("#display-name").required = state.mode === "register";
  $("#auth-title").textContent = state.mode === "register" ? "Tạo tài khoản ✨" : "Chào mừng trở lại 👋";
  $("#auth-submit").textContent = state.mode === "register" ? "Tạo tài khoản" : "Đăng nhập";
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
    showToast(state.mode === "register" ? "Đã tạo tài khoản." : "Chào mừng trở lại!");
  } catch (error) {
    showToast(translateError(error.message), "error");
  } finally {
    $("#auth-submit").disabled = false;
  }
}

// ---------------------------------------------------------------------------
// Loading data
// ---------------------------------------------------------------------------

async function loadAll() {
  await Promise.all([loadConversations(), loadFriends(), loadRequests()]);
}

async function loadConversations() {
  if (!state.token) return;
  try {
    state.conversations = await apiRequest("/conversations", { headers: authHeaders() });
    if (state.active) {
      const refreshed = findConversation(state.active.id);
      if (refreshed) state.active = refreshed;
    }
    renderConversations();
  } catch (error) {
    if (error.message === "authentication failed") clearSession();
    else showToast(translateError(error.message), "error");
  }
}

async function loadFriends() {
  if (!state.token) return;
  try {
    state.friends = await apiRequest("/friends", { headers: authHeaders() });
  } catch {
    /* ignore */
  }
}

async function loadRequests() {
  if (!state.token) return;
  try {
    state.requests = await apiRequest("/friends/requests", { headers: authHeaders() });
    renderRequests();
  } catch {
    /* ignore */
  }
}

const findConversation = (id) => state.conversations.find((c) => c.id === id) || null;

// ---------------------------------------------------------------------------
// Friend requests / add friend
// ---------------------------------------------------------------------------

async function sendFriendRequest(event) {
  event.preventDefault();
  const input = $("#friend-phone");
  const phone = input.value.trim();
  if (!phone) return;
  try {
    $("#add-friend-button").disabled = true;
    const result = await apiRequest("/friends/requests", {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ phone }),
    });
    input.value = "";
    if (result.status === "accepted") {
      showToast("Hai bạn đã là bạn bè! 🎉");
      await Promise.all([loadFriends(), loadConversations()]);
    } else {
      showToast("Đã gửi lời mời kết bạn.");
    }
  } catch (error) {
    const friendly =
      error.message === "not found"
        ? "Không tìm thấy tài khoản với số điện thoại này."
        : error.message === "resource conflict"
          ? "Hai bạn đã là bạn bè rồi."
          : translateError(error.message);
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
    showToast(accept ? "Đã chấp nhận lời mời. 🎉" : "Đã từ chối lời mời.");
    await Promise.all([loadRequests(), loadFriends()]);
  } catch (error) {
    showToast(translateError(error.message), "error");
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
    const avatar = document.createElement("span");
    avatar.className = "avatar";
    applyAvatar(avatar, request.from);
    const info = document.createElement("div");
    info.className = "request-item-info";
    const name = document.createElement("strong");
    name.textContent = request.from.display_name;
    const phone = document.createElement("span");
    phone.textContent = request.from.phone;
    info.append(name, phone);
    const actions = document.createElement("div");
    actions.className = "request-actions";
    actions.append(
      iconButton("req-btn accept", "Chấp nhận", ICONS.check, () => respondToRequest(request.id, true)),
      iconButton("req-btn decline", "Từ chối", ICONS.close, () => respondToRequest(request.id, false)),
    );
    item.append(avatar, info, actions);
    list.append(item);
  });
}

// ---------------------------------------------------------------------------
// Conversation list
// ---------------------------------------------------------------------------

const isGroup = (c) => c.kind === "group";
const conversationTitle = (c) => (isGroup(c) ? c.title : c.other_user?.display_name || "Cuộc trò chuyện");
const conversationAvatarInfo = (c) =>
  isGroup(c) ? { id: c.id, title: c.title, avatar_attachment_id: c.avatar_attachment_id } : c.other_user || { id: c.id, display_name: "?" };

function memberName(conversation, userId) {
  if (userId === state.user.id) return "Bạn";
  const member = conversation.members?.find((m) => m.id === userId);
  return member?.display_name || "Thành viên";
}

function previewText(c) {
  const last = c.last_message;
  if (!last) return isGroup(c) ? "Nhóm mới được tạo." : "Chưa có tin nhắn.";
  const prefix = last.sender_id === state.user.id ? "Bạn: " : isGroup(c) ? `${memberName(c, last.sender_id)}: ` : "";
  if (last.deleted) return `${prefix}Tin nhắn đã thu hồi`;
  if (last.kind === "image") return `${prefix}📷 Ảnh`;
  if (last.kind === "voice") return `${prefix}🎙️ Tin nhắn thoại`;
  return prefix + last.body;
}

const formatTime = (iso) => new Date(iso).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });

function formatLastSeen(iso) {
  if (!iso) return "Ngoại tuyến";
  const date = new Date(iso);
  const sameDay = date.toDateString() === new Date().toDateString();
  const when = sameDay
    ? formatTime(iso)
    : date.toLocaleString([], { month: "short", day: "numeric", hour: "2-digit", minute: "2-digit" });
  return `Hoạt động ${when}`;
}

function renderConversations() {
  const list = $("#conversation-list");
  list.replaceChildren();
  if (!state.conversations.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = state.token ? "Chưa có cuộc trò chuyện. Nhấn ✎ để bắt đầu." : "Đăng nhập để xem trò chuyện.";
    list.append(empty);
    return;
  }
  state.conversations.forEach((c) => {
    const button = document.createElement("button");
    button.type = "button";
    button.className = "friend-item";
    if (state.active?.id === c.id) button.classList.add("active");
    if (c.unread_count > 0) button.classList.add("unread");

    const wrap = document.createElement("div");
    wrap.className = "friend-avatar-wrap";
    const avatar = document.createElement("span");
    avatar.className = "avatar";
    applyAvatar(avatar, conversationAvatarInfo(c));
    wrap.append(avatar);
    if (!isGroup(c)) {
      const dot = document.createElement("span");
      dot.className = `presence-dot ${c.online ? "online" : ""}`.trim();
      wrap.append(dot);
    }

    const main = document.createElement("div");
    main.className = "friend-main";
    const top = document.createElement("div");
    top.className = "friend-top";
    const name = document.createElement("span");
    name.className = "friend-name";
    name.textContent = (isGroup(c) ? "👥 " : "") + conversationTitle(c);
    top.append(name);
    if (c.last_message) {
      const time = document.createElement("span");
      time.className = "friend-time";
      time.textContent = formatTime(c.last_message.sent_at);
      top.append(time);
    }
    const bottom = document.createElement("div");
    bottom.className = "friend-bottom";
    const preview = document.createElement("span");
    preview.className = "friend-preview";
    preview.textContent = previewText(c);
    bottom.append(preview);
    if (c.unread_count > 0) {
      const badge = document.createElement("span");
      badge.className = "unread-badge";
      badge.textContent = c.unread_count > 99 ? "99+" : String(c.unread_count);
      bottom.append(badge);
    }
    main.append(top, bottom);
    button.append(wrap, main);
    button.addEventListener("click", () => openConversation(c));
    list.append(button);
  });
}

// ---------------------------------------------------------------------------
// Active conversation
// ---------------------------------------------------------------------------

function showChatPlaceholder() {
  $("#chat-placeholder").classList.remove("hidden");
  $("#chat-area").classList.add("hidden");
  messages.replaceChildren();
  setStatus(socketStatus, "Mất kết nối");
  [messageInput, sendButton, attachButton, micButton].forEach((el) => (el.disabled = true));
}

async function openConversation(conversation) {
  state.active = conversation;
  state.replyTo = null;
  state.typingUntil = 0;
  renderReplyBanner();
  closeSearch();
  renderConversations();
  appScreen.classList.add("show-chat");
  $("#chat-placeholder").classList.add("hidden");
  $("#chat-area").classList.remove("hidden");
  $("#chat-title").textContent = conversationTitle(conversation);
  applyAvatar(chatAvatar, conversationAvatarInfo(conversation));
  $("#group-info-button").classList.toggle("hidden", !isGroup(conversation));
  messages.replaceChildren();
  messageIndex.clear();
  renderPresenceLine();

  try {
    const history = await apiRequest(`/conversations/${conversation.id}/messages`, { headers: authHeaders() });
    messages.replaceChildren();
    messageIndex.clear();
    history.forEach(appendMessage);
    refreshPinnedBar();
    markRead(conversation.id);
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
  updateComposerState();
  if (!messageInput.disabled) messageInput.focus();
  if (!state.socket || state.socket.readyState === WebSocket.CLOSED) connectSocket();
}

function markRead(conversationId) {
  sendFrame({ type: "read", conversation_id: conversationId });
  const c = findConversation(conversationId);
  if (c && c.unread_count) {
    c.unread_count = 0;
    renderConversations();
  }
}

function renderPresenceLine() {
  const c = state.active;
  if (!c) return;
  if (isGroup(c)) {
    if (Date.now() < state.typingUntil && state.typingName) {
      presenceLine.textContent = `${state.typingName} đang nhập…`;
      presenceLine.className = "presence-line typing";
    } else {
      presenceLine.textContent = `${c.members.length} thành viên`;
      presenceLine.className = "presence-line";
    }
    return;
  }
  if (Date.now() < state.typingUntil) {
    presenceLine.textContent = "đang nhập…";
    presenceLine.className = "presence-line typing";
  } else if (c.online) {
    presenceLine.textContent = "Đang hoạt động";
    presenceLine.className = "presence-line online";
  } else {
    presenceLine.textContent = formatLastSeen(c.other_user?.last_seen_at);
    presenceLine.className = "presence-line";
  }
}

function updateComposerState() {
  const ready = Boolean(state.socket && state.socket.readyState === WebSocket.OPEN && state.active);
  [messageInput, sendButton, attachButton, micButton].forEach((el) => (el.disabled = !ready));
}

// ---------------------------------------------------------------------------
// WebSocket
// ---------------------------------------------------------------------------

function connectSocket() {
  if (!state.token) return;
  if (state.socket && state.socket.readyState !== WebSocket.CLOSED) return;
  setStatus(socketStatus, "Đang kết nối", "connecting");
  const socket = new WebSocket(wsUrl(state.token));
  state.socket = socket;
  socket.addEventListener("open", () => {
    setStatus(socketStatus, "Trực tuyến", "online");
    updateComposerState();
    if (state.active) markRead(state.active.id);
  });
  socket.addEventListener("message", (event) => {
    try {
      handleSocketEvent(JSON.parse(event.data));
    } catch {
      showToast("Nhận được tin nhắn không đọc được.", "error");
    }
  });
  socket.addEventListener("close", () => {
    if (state.socket === socket) {
      state.socket = null;
      setStatus(socketStatus, "Mất kết nối");
      updateComposerState();
    }
  });
  socket.addEventListener("error", () => setStatus(socketStatus, "Lỗi", "error"));
}

function disconnectSocket() {
  if (state.socket) {
    state.socket.close();
    state.socket = null;
  }
  [messageInput, sendButton, attachButton, micButton].forEach((el) => (el.disabled = true));
  if (socketStatus) setStatus(socketStatus, "Mất kết nối");
}

function handleSocketEvent(event) {
  switch (event.type) {
    case "error":
      showToast(translateError(event.error), "error");
      return;
    case "message":
      handleIncomingMessage(event.message);
      return;
    case "message_deleted":
      handleMessageDeleted(event);
      return;
    case "message_pinned":
      handlePinned(event);
      return;
    case "read":
      handleReadEvent(event);
      return;
    case "typing":
      handleTyping(event);
      return;
    case "presence":
      handlePresence(event);
      return;
    case "reaction":
      handleReaction(event);
      return;
    case "conversation_updated":
      loadConversations();
      return;
    default:
  }
}

async function handleIncomingMessage(message) {
  let c = findConversation(message.conversation_id);
  if (!c) {
    await loadConversations();
    c = findConversation(message.conversation_id);
  }
  if (c) c.last_message = message;
  const mine = message.sender_id === state.user.id;
  const isActive = state.active && message.conversation_id === state.active.id;
  if (isActive) {
    appendMessage(message);
    if (!mine) sendFrame({ type: "read", conversation_id: message.conversation_id });
  } else if (!mine && c) {
    c.unread_count = (c.unread_count || 0) + 1;
    showToast(`Tin nhắn mới · ${conversationTitle(c)}`);
  }
  renderConversations();
}

function handleMessageDeleted(event) {
  const entry = messageIndex.get(event.message_id);
  if (entry) {
    Object.assign(entry.message, { deleted: true, body: "", attachment_id: null, duration_ms: null, reactions: [], pinned: false });
    rerenderMessage(entry);
    refreshPinnedBar();
  }
  const c = findConversation(event.conversation_id);
  if (c?.last_message?.id === event.message_id) {
    c.last_message.deleted = true;
    renderConversations();
  }
}

function handlePinned(event) {
  const entry = messageIndex.get(event.message_id);
  if (entry) {
    entry.message.pinned = event.pinned;
    rerenderMessage(entry);
  }
  refreshPinnedBar();
}

function handleReadEvent(event) {
  if (!state.active || event.conversation_id !== state.active.id) return;
  if (event.user_id === state.user.id) return;
  const at = Date.parse(event.at);
  messageIndex.forEach((entry) => {
    const m = entry.message;
    if (m.sender_id === state.user.id && Date.parse(m.sent_at) <= at && !m.read_by.includes(event.user_id)) {
      m.read_by.push(event.user_id);
      renderStatus(entry);
    }
  });
}

function handleTyping(event) {
  if (!state.active || event.conversation_id !== state.active.id) return;
  state.typingUntil = Date.now() + TYPING_SHOW_MS;
  state.typingName = memberName(state.active, event.from);
  renderPresenceLine();
  window.clearTimeout(state.typingTimer);
  state.typingTimer = window.setTimeout(renderPresenceLine, TYPING_SHOW_MS + 50);
}

function handlePresence(event) {
  let touched = false;
  state.conversations.forEach((c) => {
    if (!isGroup(c) && c.other_user?.id === event.user_id) {
      c.online = event.online;
      if (event.last_seen_at) c.other_user.last_seen_at = event.last_seen_at;
      touched = true;
    }
  });
  state.friends.forEach((f) => {
    if (f.id === event.user_id) f.online = event.online;
  });
  if (touched) {
    renderConversations();
    renderPresenceLine();
  }
}

function handleReaction(event) {
  const entry = messageIndex.get(event.message_id);
  if (!entry) return;
  const reactions = entry.message.reactions;
  let group = reactions.find((r) => r.emoji === event.emoji);
  if (event.added) {
    if (!group) {
      group = { emoji: event.emoji, user_ids: [] };
      reactions.push(group);
    }
    if (!group.user_ids.includes(event.user_id)) group.user_ids.push(event.user_id);
  } else if (group) {
    group.user_ids = group.user_ids.filter((id) => id !== event.user_id);
    if (!group.user_ids.length) entry.message.reactions = reactions.filter((r) => r !== group);
  }
  renderReactions(entry);
}

// ---------------------------------------------------------------------------
// Message rendering
// ---------------------------------------------------------------------------

const senderName = (id) => (id === state.user.id ? "Bạn" : memberName(state.active, id));

function renderStatus(entry) {
  const status = entry.element.querySelector(".ticks");
  if (!status) return;
  const m = entry.message;
  if (isGroup(state.active)) {
    const count = m.read_by.length;
    status.textContent = count > 0 ? `👁 ${count}` : "✓";
    status.className = count > 0 ? "ticks read" : "ticks";
    status.title = count > 0 ? `Đã xem bởi ${count} người` : "Đã gửi";
  } else {
    const read = state.active.other_user && m.read_by.includes(state.active.other_user.id);
    status.textContent = read ? "✓✓" : "✓";
    status.className = read ? "ticks read" : "ticks";
    status.title = read ? "Đã xem" : "Đã gửi";
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
    chip.addEventListener("click", () => sendFrame({ type: "reaction", message_id: entry.message.id, emoji: reaction.emoji }));
    container.append(chip);
  });
}

function renderImageBody(body, attachmentId) {
  const placeholder = document.createElement("span");
  placeholder.className = "image-loading";
  placeholder.textContent = "Đang tải ảnh…";
  body.append(placeholder);
  attachmentUrl(attachmentId)
    .then((url) => {
      const image = document.createElement("img");
      image.className = "message-image";
      image.alt = "Ảnh đã gửi";
      image.src = url;
      image.addEventListener("click", () => window.open(url, "_blank"));
      placeholder.replaceWith(image);
    })
    .catch(() => (placeholder.textContent = "Không tải được ảnh."));
}

function formatDuration(ms) {
  const total = Math.round((ms || 0) / 1000);
  return `${Math.floor(total / 60)}:${String(total % 60).padStart(2, "0")}`;
}

function renderVoiceBody(body, message) {
  const wrap = document.createElement("div");
  wrap.className = "voice-message";
  const icon = document.createElement("span");
  icon.className = "voice-icon";
  icon.textContent = "🎙️";
  const audio = document.createElement("audio");
  audio.controls = true;
  audio.preload = "none";
  const duration = document.createElement("span");
  duration.className = "voice-duration";
  duration.textContent = formatDuration(message.duration_ms);
  wrap.append(icon, audio, duration);
  body.append(wrap);
  attachmentUrl(message.attachment_id)
    .then((url) => (audio.src = url))
    .catch(() => (duration.textContent = "không tải được"));
}

function buildMessageElement(message) {
  const mine = message.sender_id === state.user.id;
  const element = document.createElement("article");
  element.className = `message ${mine ? "mine" : ""}`.trim();
  if (message.deleted) element.classList.add("deleted");
  if (message.pinned) element.classList.add("pinned");
  element.dataset.messageId = message.id;

  // Sender name above others' bubbles in groups.
  if (!mine && isGroup(state.active) && !message.deleted) {
    const author = document.createElement("div");
    author.className = "message-author";
    author.textContent = memberName(state.active, message.sender_id);
    element.append(author);
  }

  if (message.reply_to) {
    const quote = document.createElement("div");
    quote.className = "reply-quote";
    const name = document.createElement("strong");
    name.textContent = senderName(message.reply_to.sender_id);
    const text = document.createElement("span");
    if (message.reply_to.deleted) {
      text.textContent = "Tin nhắn đã thu hồi";
      quote.classList.add("deleted");
    } else if (message.reply_to.kind === "image") text.textContent = "📷 Ảnh";
    else if (message.reply_to.kind === "voice") text.textContent = "🎙️ Tin nhắn thoại";
    else text.textContent = message.reply_to.body;
    quote.append(name, text);
    element.append(quote);
  }

  const bubble = document.createElement("div");
  bubble.className = "bubble";
  if (message.pinned && !message.deleted) {
    const pin = document.createElement("span");
    pin.className = "pin-badge";
    pin.innerHTML = `<svg viewBox="0 0 24 24">${ICONS.pin}</svg>`;
    pin.title = "Đã ghim";
    bubble.append(pin);
  }
  const body = document.createElement("div");
  body.className = "message-body";
  if (message.deleted) body.textContent = "Tin nhắn đã được thu hồi.";
  else if (message.kind === "image" && message.attachment_id) renderImageBody(body, message.attachment_id);
  else if (message.kind === "voice" && message.attachment_id) renderVoiceBody(body, message);
  else body.textContent = message.body;
  bubble.append(body);
  element.append(bubble);

  const chips = document.createElement("div");
  chips.className = "reaction-chips";
  element.append(chips);

  const foot = document.createElement("div");
  foot.className = "message-foot";
  const time = document.createElement("time");
  time.dateTime = message.sent_at;
  time.textContent = formatTime(message.sent_at);
  foot.append(time);
  if (mine && !message.deleted) {
    const status = document.createElement("span");
    status.className = "ticks";
    foot.append(status);
  }
  element.append(foot);

  if (!message.deleted) element.append(buildMessageActions(message, element));
  return element;
}

function buildMessageActions(message, element) {
  const mine = message.sender_id === state.user.id;
  const actions = document.createElement("div");
  actions.className = "message-actions";
  actions.append(
    iconButton("message-action", "Trả lời", ICONS.reply, () => {
      state.replyTo = message;
      renderReplyBanner();
      messageInput.focus();
    }),
    iconButton("message-action", "Thả cảm xúc", ICONS.react, (e) => {
      e.stopPropagation();
      toggleReactionPicker(actions, message);
    }),
    iconButton("message-action", message.pinned ? "Bỏ ghim" : "Ghim", ICONS.pin, () => togglePin(message)),
    iconButton("message-action", "Xoá phía tôi", ICONS.hide, () => deleteMessage(message, element, "me")),
  );
  if (mine) {
    actions.append(
      iconButton("message-action danger", "Thu hồi với mọi người", ICONS.trash, () => deleteMessage(message, element, "everyone")),
    );
  }
  return actions;
}

function toggleReactionPicker(anchor, message) {
  const existing = anchor.querySelector(".reaction-picker");
  if (existing) {
    existing.remove();
    return;
  }
  document.querySelectorAll(".reaction-picker").forEach((p) => p.remove());
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
  renderStatus(entry);
  renderReactions(entry);
}

function appendMessage(message) {
  const entry = { message, element: buildMessageElement(message) };
  messageIndex.set(message.id, entry);
  messages.append(entry.element);
  renderStatus(entry);
  renderReactions(entry);
  messages.scrollTop = messages.scrollHeight;
}

async function deleteMessage(message, element, scope) {
  const confirmText =
    scope === "everyone" ? "Thu hồi tin nhắn này với mọi người? Không thể hoàn tác." : "Xoá tin nhắn này chỉ ở phía bạn?";
  if (!window.confirm(confirmText)) return;
  try {
    await apiRequest(`/messages/${message.id}?scope=${scope}`, { method: "DELETE", headers: authHeaders() });
    if (scope === "me") {
      messageIndex.delete(message.id);
      element.remove();
    }
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

async function togglePin(message) {
  try {
    await apiRequest(`/messages/${message.id}/pin`, {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ pinned: !message.pinned }),
    });
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

function refreshPinnedBar() {
  const pinned = [...messageIndex.values()].map((e) => e.message).filter((m) => m.pinned && !m.deleted);
  pinnedBar.classList.toggle("hidden", pinned.length === 0);
  if (!pinned.length) return;
  const latest = pinned[pinned.length - 1];
  const text =
    latest.kind === "image" ? "📷 Ảnh" : latest.kind === "voice" ? "🎙️ Tin nhắn thoại" : latest.body;
  $("#pinned-text").textContent = text;
  $("#pinned-count").textContent = pinned.length > 1 ? `${pinned.length} tin đã ghim` : "";
  pinnedBar.onclick = () => {
    const entry = messageIndex.get(latest.id);
    if (entry) {
      entry.element.scrollIntoView({ behavior: "smooth", block: "center" });
      entry.element.classList.add("flash");
      window.setTimeout(() => entry.element.classList.remove("flash"), 1200);
    }
  };
}

// ---------------------------------------------------------------------------
// Composer: text, image, voice, replies, typing
// ---------------------------------------------------------------------------

function renderReplyBanner() {
  if (!state.replyTo) {
    replyBanner.classList.add("hidden");
    return;
  }
  replyBanner.classList.remove("hidden");
  $("#reply-banner-name").textContent = `Trả lời ${senderName(state.replyTo.sender_id)}`;
  $("#reply-banner-body").textContent =
    state.replyTo.kind === "image" ? "📷 Ảnh" : state.replyTo.kind === "voice" ? "🎙️ Tin nhắn thoại" : state.replyTo.body;
}

function baseFrame() {
  const frame = { type: "message", conversation_id: state.active.id };
  if (state.replyTo) frame.reply_to = state.replyTo.id;
  return frame;
}

function clearReply() {
  state.replyTo = null;
  renderReplyBanner();
}

function sendMessage(event) {
  event.preventDefault();
  const body = messageInput.value.trim();
  if (!body || !state.active) return;
  if (!sendFrame({ ...baseFrame(), body })) return;
  clearReply();
  messageInput.value = "";
  messageInput.focus();
}

async function uploadAttachment(blob, contentType) {
  const upload = await apiRequest("/attachments", {
    method: "POST",
    headers: authHeaders({ "content-type": contentType }),
    body: blob,
  });
  return upload.id;
}

async function sendImage(file) {
  if (!file || !state.active) return;
  if (file.size > MAX_IMAGE_BYTES) return showToast("Ảnh phải nhỏ hơn 5 MB.", "error");
  try {
    attachButton.disabled = true;
    const id = await uploadAttachment(file, file.type || "application/octet-stream");
    sendFrame({ ...baseFrame(), kind: "image", attachment_id: id });
    clearReply();
  } catch (error) {
    showToast(error.message === "invalid request" ? "Chỉ hỗ trợ ảnh PNG, JPEG, GIF hoặc WebP." : translateError(error.message), "error");
  } finally {
    updateComposerState();
  }
}

function notifyTyping() {
  if (!state.active) return;
  const now = Date.now();
  if (now - state.lastTypingSentAt < TYPING_SEND_INTERVAL_MS) return;
  if (sendFrame({ type: "typing", conversation_id: state.active.id })) state.lastTypingSentAt = now;
}

// ----- Voice recording -----

async function startRecording() {
  if (!state.active || !navigator.mediaDevices?.getUserMedia) {
    return showToast("Trình duyệt không hỗ trợ ghi âm.", "error");
  }
  try {
    const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    const recorder = new MediaRecorder(stream);
    state.recorder = recorder;
    state.recordingChunks = [];
    state.recordingStart = Date.now();
    recorder.addEventListener("dataavailable", (e) => e.data.size && state.recordingChunks.push(e.data));
    recorder.addEventListener("stop", () => stream.getTracks().forEach((t) => t.stop()));
    recorder.start();
    $("#message-form").classList.add("hidden");
    $("#recording-bar").classList.remove("hidden");
    state.recordingTimer = window.setInterval(() => {
      const ms = Date.now() - state.recordingStart;
      $("#recording-time").textContent = formatDuration(ms);
      if (ms >= MAX_VOICE_MS) finishRecording(true);
    }, 200);
  } catch {
    showToast("Không truy cập được micro.", "error");
  }
}

function stopRecorderUI() {
  window.clearInterval(state.recordingTimer);
  $("#recording-bar").classList.add("hidden");
  $("#message-form").classList.remove("hidden");
  updateComposerState();
}

async function finishRecording(send) {
  const recorder = state.recorder;
  if (!recorder) return;
  state.recorder = null;
  const duration = Date.now() - state.recordingStart;
  await new Promise((resolve) => {
    recorder.addEventListener("stop", resolve, { once: true });
    recorder.stop();
  });
  stopRecorderUI();
  if (!send || duration < 500) return;
  const blob = new Blob(state.recordingChunks, { type: recorder.mimeType || "audio/webm" });
  try {
    const id = await uploadAttachment(blob, blob.type || "audio/webm");
    sendFrame({ ...baseFrame(), kind: "voice", attachment_id: id, duration_ms: Math.min(duration, MAX_VOICE_MS) });
    clearReply();
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

function openSearch() {
  searchBar.classList.remove("hidden");
  searchInput.value = "";
  $("#search-count").textContent = "";
  searchInput.focus();
}

function closeSearch() {
  searchBar.classList.add("hidden");
  messages.querySelectorAll(".search-hit").forEach((el) => el.classList.remove("search-hit"));
}

let searchDebounce = null;
function onSearchInput() {
  window.clearTimeout(searchDebounce);
  searchDebounce = window.setTimeout(runSearch, 250);
}

async function runSearch() {
  const q = searchInput.value.trim();
  messages.querySelectorAll(".search-hit").forEach((el) => el.classList.remove("search-hit"));
  if (!q || !state.active) {
    $("#search-count").textContent = "";
    return;
  }
  try {
    const hits = await apiRequest(`/conversations/${state.active.id}/search?q=${encodeURIComponent(q)}`, { headers: authHeaders() });
    $("#search-count").textContent = hits.length ? `${hits.length} kết quả` : "Không có kết quả";
    let firstEl = null;
    hits.forEach((hit) => {
      const entry = messageIndex.get(hit.id);
      if (entry) {
        entry.element.classList.add("search-hit");
        firstEl ||= entry.element;
      }
    });
    if (firstEl) firstEl.scrollIntoView({ behavior: "smooth", block: "center" });
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

// ---------------------------------------------------------------------------
// Avatar upload
// ---------------------------------------------------------------------------

async function changeAvatar(file) {
  if (!file) return;
  if (file.size > MAX_IMAGE_BYTES) return showToast("Ảnh phải nhỏ hơn 5 MB.", "error");
  try {
    const id = await uploadAttachment(file, file.type || "image/png");
    const updated = await apiRequest("/me/avatar", {
      method: "PUT",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ attachment_id: id }),
    });
    state.user = updated;
    sessionStorage.setItem(SESSION_USER_KEY, JSON.stringify(updated));
    blobUrlCache.delete(id);
    applyAvatar($("#avatar"), updated);
    showToast("Đã cập nhật ảnh đại diện.");
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

// ---------------------------------------------------------------------------
// New chat / group modal
// ---------------------------------------------------------------------------

function openModal() {
  state.modalTab = "direct";
  document.querySelector(".modal-tabs").classList.remove("hidden");
  renderModalTab();
  modalBackdrop.classList.remove("hidden");
}

function closeModal() {
  modalBackdrop.classList.add("hidden");
}

function renderModalTab() {
  document.querySelectorAll("[data-modal-tab]").forEach((tab) => {
    tab.classList.toggle("active", tab.dataset.modalTab === state.modalTab);
  });
  const group = state.modalTab === "group";
  $("#modal-title").textContent = group ? "Tạo nhóm" : "Tin nhắn mới";
  $("#group-title-row").classList.toggle("hidden", !group);
  $("#modal-hint").textContent = group ? "Chọn các thành viên cho nhóm." : "Chọn một người bạn để bắt đầu trò chuyện.";
  $("#create-group-button").classList.toggle("hidden", !group);
  renderPicker();
}

function renderPicker() {
  pickerList.replaceChildren();
  if (!state.friends.length) {
    const empty = document.createElement("div");
    empty.className = "empty-state";
    empty.textContent = "Chưa có bạn bè. Hãy thêm bạn trước.";
    pickerList.append(empty);
    return;
  }
  const group = state.modalTab === "group";
  state.friends.forEach((friend) => {
    const row = document.createElement(group ? "label" : "button");
    row.className = "picker-row";
    if (!group) row.type = "button";
    const avatar = document.createElement("span");
    avatar.className = "avatar small";
    applyAvatar(avatar, friend);
    const name = document.createElement("span");
    name.className = "picker-name";
    name.textContent = friend.display_name;
    row.append(avatar, name);
    if (group) {
      const checkbox = document.createElement("input");
      checkbox.type = "checkbox";
      checkbox.value = friend.id;
      checkbox.className = "picker-check";
      row.append(checkbox);
    } else {
      row.addEventListener("click", () => startDirect(friend.id));
    }
    pickerList.append(row);
  });
}

async function startDirect(userId) {
  try {
    const conversation = await apiRequest("/conversations/direct", {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ user_id: userId }),
    });
    closeModal();
    await loadConversations();
    const fresh = findConversation(conversation.id) || conversation;
    openConversation(fresh);
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

async function createGroup() {
  const title = $("#group-title").value.trim();
  const ids = [...pickerList.querySelectorAll(".picker-check:checked")].map((c) => c.value);
  if (!title) return showToast("Hãy đặt tên nhóm.", "error");
  if (!ids.length) return showToast("Chọn ít nhất một thành viên.", "error");
  try {
    const conversation = await apiRequest("/conversations/group", {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ title, member_ids: ids }),
    });
    closeModal();
    await loadConversations();
    openConversation(findConversation(conversation.id) || conversation);
    showToast("Đã tạo nhóm. 🎉");
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

// ----- Group info (members + add + leave) -----

function openGroupInfo() {
  const c = state.active;
  if (!c || !isGroup(c)) return;
  $("#modal-title").textContent = c.title;
  $("#group-title-row").classList.add("hidden");
  $("#create-group-button").classList.add("hidden");
  $("#modal-hint").textContent = `${c.members.length} thành viên`;
  document.querySelector(".modal-tabs").classList.add("hidden");
  pickerList.replaceChildren();

  c.members.forEach((member) => {
    const row = document.createElement("div");
    row.className = "picker-row";
    const avatar = document.createElement("span");
    avatar.className = "avatar small";
    applyAvatar(avatar, member);
    const name = document.createElement("span");
    name.className = "picker-name";
    name.textContent = member.id === state.user.id ? "Bạn" : member.display_name;
    if (member.id === c.created_by) name.textContent += " · chủ nhóm";
    row.append(avatar, name);
    pickerList.append(row);
  });

  // Friends not yet in the group can be added.
  const memberIds = new Set(c.members.map((m) => m.id));
  const addable = state.friends.filter((f) => !memberIds.has(f.id));
  if (addable.length) {
    const heading = document.createElement("p");
    heading.className = "requests-title";
    heading.textContent = "Thêm thành viên";
    pickerList.append(heading);
    addable.forEach((friend) => {
      const row = document.createElement("button");
      row.type = "button";
      row.className = "picker-row";
      const avatar = document.createElement("span");
      avatar.className = "avatar small";
      applyAvatar(avatar, friend);
      const name = document.createElement("span");
      name.className = "picker-name";
      name.textContent = friend.display_name;
      const plus = document.createElement("span");
      plus.className = "picker-add";
      plus.textContent = "+ Thêm";
      row.append(avatar, name, plus);
      row.addEventListener("click", () => addMember(c.id, friend.id));
      pickerList.append(row);
    });
  }

  const leave = document.createElement("button");
  leave.type = "button";
  leave.className = "leave-button";
  leave.textContent = "Rời nhóm";
  leave.addEventListener("click", () => leaveGroup(c.id));
  pickerList.append(leave);

  modalBackdrop.classList.remove("hidden");
}

async function addMember(conversationId, userId) {
  try {
    await apiRequest(`/conversations/${conversationId}/members`, {
      method: "POST",
      headers: authHeaders({ "content-type": "application/json" }),
      body: JSON.stringify({ user_id: userId }),
    });
    await loadConversations();
    closeModal();
    showToast("Đã thêm thành viên.");
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

async function leaveGroup(conversationId) {
  if (!window.confirm("Rời khỏi nhóm này?")) return;
  try {
    await apiRequest(`/conversations/${conversationId}/members/${state.user.id}`, {
      method: "DELETE",
      headers: authHeaders(),
    });
    closeModal();
    state.active = null;
    showChatPlaceholder();
    appScreen.classList.remove("show-chat");
    await loadConversations();
    showToast("Đã rời nhóm.");
  } catch (error) {
    showToast(translateError(error.message), "error");
  }
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

document.querySelectorAll(".tabs:not(.modal-tabs) .tab").forEach((tab) => {
  tab.addEventListener("click", () => {
    state.mode = tab.dataset.mode;
    renderMode();
  });
});
document.querySelectorAll("[data-modal-tab]").forEach((tab) => {
  tab.addEventListener("click", () => {
    state.modalTab = tab.dataset.modalTab;
    renderModalTab();
  });
});

$("#auth-form").addEventListener("submit", authenticate);
$("#add-friend-form").addEventListener("submit", sendFriendRequest);
$("#message-form").addEventListener("submit", sendMessage);
messageInput.addEventListener("input", notifyTyping);
attachButton.addEventListener("click", () => imageInput.click());
imageInput.addEventListener("change", () => {
  const [file] = imageInput.files;
  imageInput.value = "";
  sendImage(file);
});
micButton.addEventListener("click", startRecording);
$("#recording-send").addEventListener("click", () => finishRecording(true));
$("#recording-cancel").addEventListener("click", () => finishRecording(false));
$("#me-button").addEventListener("click", () => avatarInput.click());
avatarInput.addEventListener("change", () => {
  const [file] = avatarInput.files;
  avatarInput.value = "";
  changeAvatar(file);
});
$("#new-chat-button").addEventListener("click", openModal);
$("#modal-close").addEventListener("click", closeModal);
modalBackdrop.addEventListener("click", (e) => {
  if (e.target === modalBackdrop) closeModal();
});
$("#create-group-button").addEventListener("click", createGroup);
$("#group-info-button").addEventListener("click", openGroupInfo);
$("#search-toggle").addEventListener("click", () => (searchBar.classList.contains("hidden") ? openSearch() : closeSearch()));
$("#search-close").addEventListener("click", closeSearch);
searchInput.addEventListener("input", onSearchInput);
$("#back-button").addEventListener("click", () => appScreen.classList.remove("show-chat"));
$("#cancel-reply").addEventListener("click", clearReply);
$("#refresh-button").addEventListener("click", loadAll);
$("#logout-button").addEventListener("click", clearSession);
document.addEventListener("keydown", (e) => {
  if (e.key === "Escape") {
    if (!modalBackdrop.classList.contains("hidden")) closeModal();
    else if (state.replyTo) clearReply();
    else if (!searchBar.classList.contains("hidden")) closeSearch();
  }
});
document.addEventListener("click", (e) => {
  if (!e.target.closest(".reaction-picker") && !e.target.closest(".message-action")) {
    document.querySelectorAll(".reaction-picker").forEach((p) => p.remove());
  }
});

renderMode();
renderSession();
if (state.token && state.user) {
  connectSocket();
  loadAll();
}
