import { initializeApp } from "https://www.gstatic.com/firebasejs/12.15.0/firebase-app.js";
import {
  GithubAuthProvider,
  getAuth,
  onAuthStateChanged,
  signInWithPopup,
  signOut
} from "https://www.gstatic.com/firebasejs/12.15.0/firebase-auth.js";
import {
  collection,
  doc,
  getFirestore,
  limit,
  onSnapshot,
  orderBy,
  query
} from "https://www.gstatic.com/firebasejs/12.15.0/firebase-firestore.js";
import {
  connectFunctionsEmulator,
  getFunctions,
  httpsCallable
} from "https://www.gstatic.com/firebasejs/12.15.0/firebase-functions.js";
import {
  getDownloadURL,
  getStorage,
  ref,
  uploadBytes
} from "https://www.gstatic.com/firebasejs/12.15.0/firebase-storage.js";
import {
  getMessaging,
  getToken,
  isSupported
} from "https://www.gstatic.com/firebasejs/12.15.0/firebase-messaging.js";

import { chatConfig, firebaseConfig } from "./chat-config.js";

const TEXT = {
  en: {
    authTitle: "GitHub sign-in required",
    authBody: "Chat uses Firebase Authentication and GitHub identity. Messages are stored in Firestore.",
    signIn: "Sign in with GitHub",
    signOut: "Sign out",
    displayName: "Display name",
    saveName: "Save",
    joinSite: "Join site discussion",
    targetUid: "User UID",
    startDirect: "Start direct chat",
    groupTitle: "Group title",
    createGroup: "Create group",
    enableNotifications: "Enable notifications",
    deleteAccount: "Delete account",
    image: "Image",
    send: "Send",
    markRead: "Mark read",
    anonymousOn: "Use anonymous name",
    anonymousOff: "Use normal name",
    createInvite: "Invite link",
    noSessions: "No conversations yet.",
    chooseThread: "Choose or create a conversation.",
    messagePlaceholder: "Write a message",
    signedIn: "Signed in.",
    signedOut: "Signed out.",
    profileReady: "Profile ready.",
    loading: "Loading chat...",
    sending: "Sending...",
    sent: "Message sent.",
    nameSaved: "Display name updated.",
    joined: "Discussion joined.",
    notificationsEnabled: "Notifications enabled for this browser.",
    notificationsUnsupported: "Notifications are not supported in this browser.",
    permissionDenied: "Notification permission was not granted.",
    directCreated: "Direct conversation ready.",
    groupCreated: "Group created.",
    readMarked: "Thread marked as read.",
    inviteCreated: "Invite link copied.",
    inviteTitle: "Invite detected",
    inviteBody: "Confirm to join this group chat.",
    acceptInvite: "Join group",
    inviteAccepted: "Joined group.",
    anonymousUpdated: "Anonymous mode updated.",
    deleteConfirm: "Delete your Eva-CLI chat account and all chat data? This cannot be undone.",
    deleteRequested: "Account deletion started.",
    errorPrefix: "Chat error:"
  },
  "zh-CN": {
    authTitle: "需要 GitHub 登录",
    authBody: "聊天功能使用 Firebase Authentication 和 GitHub 身份，消息保存在 Firestore。",
    signIn: "使用 GitHub 登录",
    signOut: "退出",
    displayName: "显示名称",
    saveName: "保存",
    joinSite: "加入官网讨论区",
    targetUid: "用户 UID",
    startDirect: "发起单聊",
    groupTitle: "群聊名称",
    createGroup: "创建群聊",
    enableNotifications: "开启通知",
    deleteAccount: "注销账号",
    image: "图片",
    send: "发送",
    markRead: "标为已读",
    anonymousOn: "使用匿名名",
    anonymousOff: "使用普通名",
    createInvite: "邀请链接",
    noSessions: "还没有会话。",
    chooseThread: "请选择或创建一个会话。",
    messagePlaceholder: "输入消息",
    signedIn: "已登录。",
    signedOut: "已退出。",
    profileReady: "用户资料已就绪。",
    loading: "正在加载聊天...",
    sending: "正在发送...",
    sent: "消息已发送。",
    nameSaved: "显示名称已更新。",
    joined: "已加入讨论区。",
    notificationsEnabled: "此浏览器已开启通知。",
    notificationsUnsupported: "此浏览器不支持通知。",
    permissionDenied: "未获得浏览器通知权限。",
    directCreated: "单聊已就绪。",
    groupCreated: "群聊已创建。",
    readMarked: "会话已标为已读。",
    inviteCreated: "邀请链接已复制。",
    inviteTitle: "检测到邀请链接",
    inviteBody: "确认后将加入该群聊。",
    acceptInvite: "加入群聊",
    inviteAccepted: "已加入群聊。",
    anonymousUpdated: "匿名模式已更新。",
    deleteConfirm: "要注销 Eva-CLI 聊天账号并删除所有聊天数据吗？此操作无法撤销。",
    deleteRequested: "账号注销任务已开始。",
    errorPrefix: "聊天错误："
  }
};

const appRoot = document.querySelector("[data-eva-chat]");

if (appRoot) {
  bootChat(appRoot).catch((error) => {
    console.error(error);
    const status = appRoot.querySelector("[data-chat-status]");
    if (status) {
      status.textContent = `${getLocaleText(appRoot).errorPrefix} ${formatError(error)}`;
      status.dataset.state = "error";
    }
  });
}

async function bootChat(root) {
  const locale = root.dataset.locale === "zh-CN" ? "zh-CN" : "en";
  const text = TEXT[locale];
  const state = {
    locale,
    text,
    user: null,
    profile: null,
    selectedThreadId: getInitialThreadId(),
    currentMember: null,
    unsubscribers: []
  };

  const els = collectElements(root);
  applyStaticText(els, text);
  setStatus(els, text.loading);

  const app = initializeApp(firebaseConfig);
  const auth = getAuth(app);
  const db = getFirestore(app);
  const storage = getStorage(app);
  const functions = getFunctions(app, chatConfig.functionsRegion);

  if (window.location.hostname === "localhost" && window.location.search.includes("firebaseEmulator=1")) {
    connectFunctionsEmulator(functions, "127.0.0.1", 5001);
  }

  const call = {
    ensureUserProfile: httpsCallable(functions, "ensureUserProfile"),
    updateDisplayName: httpsCallable(functions, "updateDisplayName"),
    createDirectThread: httpsCallable(functions, "createDirectThread"),
    createGroup: httpsCallable(functions, "createGroup"),
    createGroupInvite: httpsCallable(functions, "createGroupInvite"),
    acceptGroupInvite: httpsCallable(functions, "acceptGroupInvite"),
    setAnonymousMode: httpsCallable(functions, "setAnonymousMode"),
    sendMessage: httpsCallable(functions, "sendMessage"),
    registerNotificationToken: httpsCallable(functions, "registerNotificationToken"),
    markThreadRead: httpsCallable(functions, "markThreadRead"),
    joinSiteDiscussion: httpsCallable(functions, "joinSiteDiscussion"),
    deleteAccount: httpsCallable(functions, "deleteAccount")
  };

  bindEvents({ app, auth, call, db, els, root, state, storage });

  onAuthStateChanged(auth, async (user) => {
    cleanupSubscriptions(state);
    state.user = user;
    state.profile = null;
    state.currentMember = null;
    state.selectedThreadId = getInitialThreadId();

    if (!user) {
      renderSignedOut(els, text);
      setStatus(els, text.signedOut);
      return;
    }

    renderSignedInShell(els);
    setStatus(els, text.loading);
    const result = await call.ensureUserProfile({});
    state.profile = result.data?.profile || null;
    renderProfile(els, user, state.profile);
    setStatus(els, text.profileReady);
    subscribeSessions({ db, els, state });
    subscribeThread({ db, els, state });
  });
}

function collectElements(root) {
  return {
    root,
    status: root.querySelector("[data-chat-status]"),
    authPanel: root.querySelector("[data-chat-auth]"),
    authTitle: root.querySelector("[data-chat-auth-title]"),
    authBody: root.querySelector("[data-chat-auth-body]"),
    shell: root.querySelector("[data-chat-shell]"),
    avatar: root.querySelector("[data-chat-avatar]"),
    displayName: root.querySelector("[data-chat-display-name]"),
    uid: root.querySelector("[data-chat-uid]"),
    nameForm: root.querySelector("[data-chat-name-form]"),
    directForm: root.querySelector("[data-chat-direct-form]"),
    groupForm: root.querySelector("[data-chat-group-form]"),
    sessions: root.querySelector("[data-chat-sessions]"),
    messages: root.querySelector("[data-chat-messages]"),
    composer: root.querySelector("[data-chat-composer]"),
    threadKind: root.querySelector("[data-chat-thread-kind]"),
    threadTitle: root.querySelector("[data-chat-thread-title]"),
    invitePanel: root.querySelector("[data-chat-invite-panel]"),
    inviteTitle: root.querySelector("[data-chat-invite-title]"),
    inviteBody: root.querySelector("[data-chat-invite-body]"),
    actions: {
      signIn: root.querySelector("[data-chat-action='sign-in']"),
      signOut: root.querySelector("[data-chat-action='sign-out']"),
      joinSite: root.querySelector("[data-chat-action='join-site']"),
      enableNotifications: root.querySelector("[data-chat-action='enable-notifications']"),
      deleteAccount: root.querySelector("[data-chat-action='delete-account']"),
      markRead: root.querySelector("[data-chat-action='mark-read']"),
      toggleAnonymous: root.querySelector("[data-chat-action='toggle-anonymous']"),
      createInvite: root.querySelector("[data-chat-action='create-invite']"),
      acceptInvite: root.querySelector("[data-chat-action='accept-invite']")
    }
  };
}

function applyStaticText(els, text) {
  els.authTitle.textContent = text.authTitle;
  els.authBody.textContent = text.authBody;
  els.actions.signIn.textContent = text.signIn;
  els.actions.signOut.title = text.signOut;
  els.actions.signOut.setAttribute("aria-label", text.signOut);
  els.actions.joinSite.textContent = text.joinSite;
  els.actions.enableNotifications.textContent = text.enableNotifications;
  els.actions.deleteAccount.textContent = text.deleteAccount;
  els.actions.markRead.textContent = text.markRead;
  els.actions.createInvite.textContent = text.createInvite;
  els.actions.acceptInvite.textContent = text.acceptInvite;
  els.inviteTitle.textContent = text.inviteTitle;
  els.inviteBody.textContent = text.inviteBody;
  els.threadKind.textContent = "";
  els.threadTitle.textContent = text.chooseThread;
  els.composer.elements.text.placeholder = text.messagePlaceholder;

  for (const label of els.root.querySelectorAll("[data-chat-label]")) {
    const key = label.dataset.chatLabel;
    label.textContent = text[key] || key;
  }
}

function bindEvents({ app, auth, call, db, els, root, state, storage }) {
  els.actions.signIn.addEventListener("click", async () => {
    const provider = new GithubAuthProvider();
    provider.addScope("read:user");
    setStatus(els, state.text.loading);
    await signInWithPopup(auth, provider);
    setStatus(els, state.text.signedIn);
  });

  els.actions.signOut.addEventListener("click", async () => {
    await signOut(auth);
  });

  els.actions.joinSite.addEventListener("click", async () => {
    const result = await call.joinSiteDiscussion({
      locale: state.locale,
      title: chatConfig.defaultThreadTitle[state.locale] || chatConfig.defaultThreadTitle.en
    });
    selectThread(result.data?.threadId, { db, els, state });
    setStatus(els, state.text.joined);
  });

  els.nameForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const displayName = els.nameForm.elements.displayName.value.trim();
    await call.updateDisplayName({ displayName });
    state.profile = { ...state.profile, displayName };
    renderProfile(els, state.user, state.profile);
    setStatus(els, state.text.nameSaved);
  });

  els.directForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const targetUid = els.directForm.elements.targetUid.value.trim();
    const result = await call.createDirectThread({ targetUid });
    selectThread(result.data?.threadId, { db, els, state });
    els.directForm.reset();
    setStatus(els, state.text.directCreated);
  });

  els.groupForm.addEventListener("submit", async (event) => {
    event.preventDefault();
    const title = els.groupForm.elements.title.value.trim();
    const result = await call.createGroup({ title });
    selectThread(result.data?.threadId, { db, els, state });
    els.groupForm.reset();
    setStatus(els, state.text.groupCreated);
  });

  els.actions.enableNotifications.addEventListener("click", async () => {
    await enableNotifications({ app, call, state });
    setStatus(els, state.text.notificationsEnabled);
  });

  els.actions.deleteAccount.addEventListener("click", async () => {
    if (!window.confirm(state.text.deleteConfirm)) {
      return;
    }
    await call.deleteAccount({});
    setStatus(els, state.text.deleteRequested);
    await signOut(auth);
  });

  els.actions.markRead.addEventListener("click", async () => {
    if (!state.selectedThreadId) {
      return;
    }
    await call.markThreadRead({ threadId: state.selectedThreadId });
    setStatus(els, state.text.readMarked);
  });

  els.actions.toggleAnonymous.addEventListener("click", async () => {
    if (!state.selectedThreadId) {
      return;
    }
    const enabled = !state.currentMember?.anonymous?.enabled;
    await call.setAnonymousMode({ threadId: state.selectedThreadId, enabled });
    setStatus(els, state.text.anonymousUpdated);
  });

  els.actions.createInvite.addEventListener("click", async () => {
    if (!state.selectedThreadId) {
      return;
    }
    const result = await call.createGroupInvite({ threadId: state.selectedThreadId });
    const url = result.data?.url;
    if (url) {
      await navigator.clipboard?.writeText(url);
      setStatus(els, `${state.text.inviteCreated} ${url}`);
    }
  });

  els.actions.acceptInvite.addEventListener("click", async () => {
    const token = getInviteToken();
    if (!token) {
      return;
    }
    const result = await call.acceptGroupInvite({ token });
    selectThread(result.data?.threadId, { db, els, state });
    els.invitePanel.hidden = true;
    setStatus(els, state.text.inviteAccepted);
  });

  els.composer.addEventListener("submit", async (event) => {
    event.preventDefault();
    if (!state.selectedThreadId) {
      return;
    }

    const text = els.composer.elements.text.value.trim();
    const image = els.composer.elements.image.files[0];
    if (!text && !image) {
      return;
    }

    setStatus(els, state.text.sending);
    let imageAttachment = null;
    if (image) {
      imageAttachment = await uploadPendingImage({ image, state, storage });
    }

    await call.sendMessage({
      threadId: state.selectedThreadId,
      text,
      image: imageAttachment
    });
    els.composer.reset();
    setStatus(els, state.text.sent);
  });

  root.addEventListener("click", (event) => {
    const button = event.target.closest("[data-chat-thread-id]");
    if (!button) {
      return;
    }
    selectThread(button.dataset.chatThreadId, { db, els, state });
  });

  if (getInviteToken()) {
    els.invitePanel.hidden = false;
  }
}

function subscribeSessions({ db, els, state }) {
  const sessionsQuery = query(
    collection(db, "users", state.user.uid, "sessions"),
    orderBy("lastMessageAt", "desc"),
    limit(40)
  );

  const unsubscribe = onSnapshot(sessionsQuery, (snapshot) => {
    const sessions = snapshot.docs.map((docSnapshot) => ({
      id: docSnapshot.id,
      ...docSnapshot.data()
    }));
    renderSessions(els, state, sessions);

    if (!state.selectedThreadId && sessions.length > 0) {
      selectThread(sessions[0].threadId || sessions[0].id, { db, els, state });
    }
  }, (error) => showError(els, state, error));

  state.unsubscribers.push(unsubscribe);
}

function subscribeThread({ db, els, state }) {
  if (!state.selectedThreadId || !state.user) {
    renderEmptyThread(els, state.text);
    return;
  }

  state.threadUnsubscribe?.();
  state.memberUnsubscribe?.();

  const messagesQuery = query(
    collection(db, "threads", state.selectedThreadId, "messages"),
    orderBy("createdAt", "asc"),
    limit(80)
  );

  state.threadUnsubscribe = onSnapshot(messagesQuery, (snapshot) => {
    const messages = snapshot.docs.map((docSnapshot) => ({
      id: docSnapshot.id,
      ...docSnapshot.data()
    }));
    renderMessages(els, state, messages);
  }, (error) => showError(els, state, error));

  state.memberUnsubscribe = onSnapshot(
    doc(db, "threads", state.selectedThreadId, "members", state.user.uid),
    (snapshot) => {
      state.currentMember = snapshot.exists() ? snapshot.data() : null;
      renderThreadActions(els, state);
    },
    (error) => showError(els, state, error)
  );
}

function selectThread(threadId, { db, els, state }) {
  if (!threadId) {
    return;
  }
  state.selectedThreadId = threadId;
  window.history.replaceState(null, "", withThreadHash(threadId));
  subscribeThread({ db, els, state });
}

function renderSignedOut(els, text) {
  els.authPanel.hidden = false;
  els.shell.hidden = true;
  els.sessions.innerHTML = "";
  renderEmptyThread(els, text);
}

function renderSignedInShell(els) {
  els.authPanel.hidden = true;
  els.shell.hidden = false;
}

function renderProfile(els, user, profile) {
  const displayName = profile?.displayName || user.displayName || user.uid.slice(0, 8);
  els.displayName.textContent = displayName;
  els.uid.textContent = user.uid;
  els.avatar.src = profile?.photoURL || user.photoURL || "/assets/eva-cli-logo.svg";
  els.nameForm.elements.displayName.value = displayName;
}

function renderSessions(els, state, sessions) {
  if (sessions.length === 0) {
    els.sessions.innerHTML = `<p class="chat-empty">${escapeHtml(state.text.noSessions)}</p>`;
    return;
  }

  els.sessions.innerHTML = sessions.map((session) => {
    const isSelected = (session.threadId || session.id) === state.selectedThreadId;
    const title = session.title || session.peerDisplayName || session.threadId || session.id;
    const preview = session.lastMessage?.text || session.lastMessage?.type || "";
    const unread = Number(session.unreadCount || 0);
    return `
      <button class="chat-session${isSelected ? " is-active" : ""}" type="button" data-chat-thread-id="${escapeHtml(session.threadId || session.id)}" role="listitem">
        <span>
          <strong>${escapeHtml(title)}</strong>
          <small>${escapeHtml(preview)}</small>
        </span>
        ${unread > 0 ? `<em>${unread}</em>` : ""}
      </button>
    `;
  }).join("");
}

function renderEmptyThread(els, text) {
  els.threadKind.textContent = "";
  els.threadTitle.textContent = text.chooseThread;
  els.messages.innerHTML = `<p class="chat-empty">${escapeHtml(text.chooseThread)}</p>`;
  els.composer.hidden = true;
}

function renderMessages(els, state, messages) {
  els.composer.hidden = false;
  els.threadTitle.textContent = state.selectedThreadId;

  if (messages.length === 0) {
    els.messages.innerHTML = `<p class="chat-empty">${escapeHtml(state.text.messagePlaceholder)}</p>`;
    return;
  }

  els.messages.innerHTML = messages.map((message) => {
    const isMine = message.senderUid === state.user.uid;
    const createdAt = formatTimestamp(message.createdAt);
    const image = message.attachments?.[0]?.downloadURL
      ? `<img class="chat-message-image" src="${escapeHtml(message.attachments[0].downloadURL)}" alt="">`
      : "";
    const body = message.text ? `<p>${escapeHtml(message.text)}</p>` : "";
    return `
      <article class="chat-message${isMine ? " is-mine" : ""}">
        <header>
          <strong>${escapeHtml(message.senderDisplayName || message.senderUid || "")}</strong>
          <time>${escapeHtml(createdAt)}</time>
        </header>
        ${body}
        ${image}
      </article>
    `;
  }).join("");
  els.messages.scrollTop = els.messages.scrollHeight;
}

function renderThreadActions(els, state) {
  const type = state.currentMember?.threadType || "";
  els.threadKind.textContent = type || "thread";
  const anonymousEnabled = Boolean(state.currentMember?.anonymous?.enabled);
  els.actions.toggleAnonymous.textContent = anonymousEnabled ? state.text.anonymousOff : state.text.anonymousOn;
}

async function uploadPendingImage({ image, state, storage }) {
  if (image.size > 5 * 1024 * 1024) {
    throw new Error("Image must be 5 MB or smaller.");
  }
  if (!image.type.startsWith("image/")) {
    throw new Error("Only image uploads are supported.");
  }

  const messageId = crypto.randomUUID();
  const safeName = image.name.replace(/[^A-Za-z0-9._-]/g, "_").slice(0, 80) || "image";
  const storagePath = `chat/${state.selectedThreadId}/${messageId}/${safeName}`;
  const storageRef = ref(storage, storagePath);
  await uploadBytes(storageRef, image, {
    contentType: image.type,
    customMetadata: {
      ownerUid: state.user.uid,
      threadId: state.selectedThreadId,
      messageId
    }
  });
  const downloadURL = await getDownloadURL(storageRef);
  return {
    messageId,
    storagePath,
    downloadURL,
    contentType: image.type,
    size: image.size
  };
}

async function enableNotifications({ app, call, state }) {
  if (!("Notification" in window) || !(await isSupported())) {
    throw new Error(state.text.notificationsUnsupported);
  }

  const permission = await Notification.requestPermission();
  if (permission !== "granted") {
    throw new Error(state.text.permissionDenied);
  }

  const registration = await navigator.serviceWorker.register("/firebase-messaging-sw.js");
  const messaging = getMessaging(app);
  const token = await getToken(messaging, {
    vapidKey: chatConfig.fcmVapidKey,
    serviceWorkerRegistration: registration
  });
  await call.registerNotificationToken({
    token,
    permission,
    platform: navigator.platform || "web",
    userAgent: navigator.userAgent
  });
}

function cleanupSubscriptions(state) {
  for (const unsubscribe of state.unsubscribers) {
    unsubscribe();
  }
  state.unsubscribers = [];
  state.threadUnsubscribe?.();
  state.memberUnsubscribe?.();
  state.threadUnsubscribe = null;
  state.memberUnsubscribe = null;
}

function getInitialThreadId() {
  const hash = new URLSearchParams(window.location.hash.replace(/^#discussion&?/, ""));
  return hash.get("thread") || null;
}

function getInviteToken() {
  const params = new URLSearchParams(window.location.search);
  return params.get("token");
}

function withThreadHash(threadId) {
  const encoded = encodeURIComponent(threadId);
  return `${window.location.pathname}${window.location.search}#discussion&thread=${encoded}`;
}

function setStatus(els, message) {
  els.status.textContent = message || "";
  delete els.status.dataset.state;
}

function showError(els, state, error) {
  console.error(error);
  els.status.textContent = `${state.text.errorPrefix} ${formatError(error)}`;
  els.status.dataset.state = "error";
}

function formatError(error) {
  return error?.message || error?.code || String(error);
}

function formatTimestamp(timestamp) {
  const date = timestamp?.toDate ? timestamp.toDate() : null;
  if (!date) {
    return "";
  }
  return new Intl.DateTimeFormat(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit"
  }).format(date);
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll("\"", "&quot;")
    .replaceAll("'", "&#039;");
}

function getLocaleText(root) {
  return TEXT[root.dataset.locale === "zh-CN" ? "zh-CN" : "en"];
}
