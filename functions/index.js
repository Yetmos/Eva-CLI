"use strict";

const crypto = require("crypto");
const admin = require("firebase-admin");
const { HttpsError, onCall } = require("firebase-functions/v2/https");
const { onDocumentCreated } = require("firebase-functions/v2/firestore");

admin.initializeApp();

const STORAGE_BUCKET = "eva-cli-7ad30.firebasestorage.app";
const db = admin.firestore();
const bucket = admin.storage().bucket(STORAGE_BUCKET);
const rtdb = admin.database();
const FieldValue = admin.firestore.FieldValue;
const Timestamp = admin.firestore.Timestamp;
const REGION = "us-central1";
const SITE_ORIGIN = "https://www.eva-cli.com";
const SITE_THREAD_ID = "group_site_discussion";
const MAX_TEXT_LENGTH = 2000;
const MAX_GROUP_TITLE_LENGTH = 48;
const MAX_DISPLAY_NAME_LENGTH = 24;
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;
const CHINESE_NAMES = ["晴川", "知言", "松月", "安澜", "云舟", "南星", "听澜", "竹白", "星河", "秋序"];

const callableOptions = {
  region: REGION,
  enforceAppCheck: false
};

exports.ensureUserProfile = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const userRecord = await admin.auth().getUser(uid);
  const provider = userRecord.providerData.find((item) => item.providerId === "github.com");
  const userRef = db.collection("users").doc(uid);

  await db.runTransaction(async (transaction) => {
    const snapshot = await transaction.get(userRef);
    const now = FieldValue.serverTimestamp();
    const providerPatch = {
      githubProviderUid: provider?.uid || null,
      photoURL: userRecord.photoURL || provider?.photoURL || null,
      emailVerified: Boolean(userRecord.emailVerified),
      updatedAt: now
    };

    if (snapshot.exists) {
      transaction.set(userRef, providerPatch, { merge: true });
      return;
    }

    transaction.set(userRef, {
      displayName: pickInitialDisplayName(uid),
      status: "active",
      createdAt: now,
      notificationSettings: {
        enabled: false,
        preview: false
      },
      deletion: {
        status: "none"
      },
      ...providerPatch
    });
  });

  const profile = await userRef.get();
  return { profile: serializeSnapshot(profile) };
});

exports.updateDisplayName = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const displayName = normalizeDisplayName(request.data?.displayName);
  await ensureActiveUser(uid);

  await db.collection("users").doc(uid).set({
    displayName,
    updatedAt: FieldValue.serverTimestamp()
  }, { merge: true });

  return { displayName };
});

exports.joinSiteDiscussion = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const profile = await ensureActiveUser(uid);
  const title = normalizeTitle(request.data?.title || "Eva-CLI Discussion");
  const threadRef = db.collection("threads").doc(SITE_THREAD_ID);
  const memberRef = threadRef.collection("members").doc(uid);

  await db.runTransaction(async (transaction) => {
    const thread = await transaction.get(threadRef);
    const member = await transaction.get(memberRef);
    const now = FieldValue.serverTimestamp();
    if (!thread.exists) {
      transaction.set(threadRef, {
        type: "group",
        title,
        createdBy: uid,
        createdAt: now,
        memberCount: 1,
        settings: {
          publicSiteThread: true,
          invitesEnabled: true
        },
        anonymousPolicy: {
          enabled: true
        },
        lastMessage: null,
        lastMessageAt: now
      });
    }

    if (!member.exists) {
      transaction.set(memberRef, memberData({ uid, role: thread.exists ? "member" : "admin", threadType: "group" }));
      if (thread.exists) {
        transaction.update(threadRef, { memberCount: FieldValue.increment(1) });
      }
    } else if (member.data().state !== "active") {
      transaction.set(memberRef, { state: "active", joinedAt: now }, { merge: true });
      if (thread.exists) {
        transaction.update(threadRef, { memberCount: FieldValue.increment(1) });
      }
    }

    transaction.set(sessionRef(uid, SITE_THREAD_ID), sessionData({
      threadId: SITE_THREAD_ID,
      type: "group",
      title,
      avatar: null,
      lastMessage: thread.exists ? thread.data().lastMessage : null,
      lastMessageAt: thread.exists ? thread.data().lastMessageAt : now,
      unreadCount: 0,
      includeDefaults: true
    }), { merge: true });
  });

  return {
    threadId: SITE_THREAD_ID,
    displayName: profile.displayName
  };
});

exports.createDirectThread = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const targetUid = normalizeUid(request.data?.targetUid);
  if (targetUid === uid) {
    throw new HttpsError("invalid-argument", "Cannot create a direct thread with yourself.");
  }

  const [profile, targetProfile] = await Promise.all([
    ensureActiveUser(uid),
    ensureActiveUser(targetUid)
  ]);

  const threadId = directThreadId(uid, targetUid);
  const threadRef = db.collection("threads").doc(threadId);

  await db.runTransaction(async (transaction) => {
    const snapshot = await transaction.get(threadRef);
    const now = FieldValue.serverTimestamp();
    if (!snapshot.exists) {
      transaction.set(threadRef, {
        type: "direct",
        title: null,
        createdBy: uid,
        createdAt: now,
        memberCount: 2,
        memberUids: [uid, targetUid].sort(),
        settings: {
          invitesEnabled: false
        },
        anonymousPolicy: {
          enabled: false
        },
        lastMessage: null,
        lastMessageAt: now
      });
      transaction.set(threadRef.collection("members").doc(uid), memberData({ uid, role: "member", threadType: "direct" }));
      transaction.set(threadRef.collection("members").doc(targetUid), memberData({ uid: targetUid, role: "member", threadType: "direct" }));
    }

    transaction.set(sessionRef(uid, threadId), sessionData({
      threadId,
      type: "direct",
      title: targetProfile.displayName,
      avatar: targetProfile.photoURL || null,
      lastMessage: snapshot.exists ? snapshot.data().lastMessage : null,
      lastMessageAt: snapshot.exists ? snapshot.data().lastMessageAt : now,
      unreadCount: 0,
      includeDefaults: true
    }), { merge: true });
    transaction.set(sessionRef(targetUid, threadId), sessionData({
      threadId,
      type: "direct",
      title: profile.displayName,
      avatar: profile.photoURL || null,
      lastMessage: snapshot.exists ? snapshot.data().lastMessage : null,
      lastMessageAt: snapshot.exists ? snapshot.data().lastMessageAt : now,
      unreadCount: 0,
      includeDefaults: true
    }), { merge: true });
  });

  return { threadId };
});

exports.createGroup = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  await ensureActiveUser(uid);
  const title = normalizeTitle(request.data?.title);
  const threadRef = db.collection("threads").doc();
  const now = FieldValue.serverTimestamp();

  await db.runTransaction(async (transaction) => {
    transaction.set(threadRef, {
      type: "group",
      title,
      createdBy: uid,
      createdAt: now,
      memberCount: 1,
      settings: {
        invitesEnabled: true
      },
      anonymousPolicy: {
        enabled: true
      },
      lastMessage: null,
      lastMessageAt: now
    });
    transaction.set(threadRef.collection("members").doc(uid), memberData({ uid, role: "admin", threadType: "group" }));
    transaction.set(sessionRef(uid, threadRef.id), sessionData({
      threadId: threadRef.id,
      type: "group",
      title,
      avatar: null,
      lastMessage: null,
      lastMessageAt: now,
      unreadCount: 0,
      includeDefaults: true
    }));
  });

  return { threadId: threadRef.id };
});

exports.createGroupInvite = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const threadId = normalizeThreadId(request.data?.threadId);
  const { thread, member } = await requireThreadMember(threadId, uid);

  if (thread.type !== "group") {
    throw new HttpsError("failed-precondition", "Only group threads support invites.");
  }
  if (member.role !== "admin") {
    throw new HttpsError("permission-denied", "Only group admins can create invites.");
  }

  const token = crypto.randomBytes(32).toString("base64url");
  const tokenHash = hashToken(token);
  const expiresAt = Timestamp.fromMillis(Date.now() + 7 * 24 * 60 * 60 * 1000);
  const maxUses = clampInteger(request.data?.maxUses, 1, 100, 20);

  await db.collection("invites").doc(tokenHash).set({
    threadId,
    createdBy: uid,
    createdAt: FieldValue.serverTimestamp(),
    expiresAt,
    maxUses,
    uses: 0,
    revokedAt: null
  });

  return {
    token,
    url: `${SITE_ORIGIN}/?token=${encodeURIComponent(token)}#discussion`
  };
});

exports.acceptGroupInvite = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  await ensureActiveUser(uid);
  const token = normalizeToken(request.data?.token);
  const tokenHash = hashToken(token);
  const inviteRef = db.collection("invites").doc(tokenHash);

  let threadId = null;
  await db.runTransaction(async (transaction) => {
    const inviteSnapshot = await transaction.get(inviteRef);
    if (!inviteSnapshot.exists) {
      throw new HttpsError("not-found", "Invite not found.");
    }

    const invite = inviteSnapshot.data();
    if (invite.revokedAt) {
      throw new HttpsError("failed-precondition", "Invite was revoked.");
    }
    if (invite.expiresAt?.toMillis() < Date.now()) {
      throw new HttpsError("deadline-exceeded", "Invite expired.");
    }
    if (Number(invite.uses || 0) >= Number(invite.maxUses || 1)) {
      throw new HttpsError("resource-exhausted", "Invite is exhausted.");
    }

    threadId = invite.threadId;
    const threadRef = db.collection("threads").doc(threadId);
    const threadSnapshot = await transaction.get(threadRef);
    if (!threadSnapshot.exists || threadSnapshot.data().type !== "group") {
      throw new HttpsError("not-found", "Group not found.");
    }

    const memberRef = threadRef.collection("members").doc(uid);
    const memberSnapshot = await transaction.get(memberRef);
    const now = FieldValue.serverTimestamp();
    if (!memberSnapshot.exists) {
      transaction.set(memberRef, memberData({ uid, role: "member", threadType: "group" }));
      transaction.update(threadRef, { memberCount: FieldValue.increment(1) });
      transaction.update(inviteRef, { uses: FieldValue.increment(1), lastUsedAt: now });
    } else if (memberSnapshot.data().state !== "active") {
      transaction.set(memberRef, { state: "active", joinedAt: now }, { merge: true });
      transaction.update(threadRef, { memberCount: FieldValue.increment(1) });
    }

    transaction.set(sessionRef(uid, threadId), sessionData({
      threadId,
      type: "group",
      title: threadSnapshot.data().title,
      avatar: null,
      lastMessage: threadSnapshot.data().lastMessage || null,
      lastMessageAt: threadSnapshot.data().lastMessageAt || now,
      unreadCount: 0,
      includeDefaults: true
    }), { merge: true });
  });

  return { threadId };
});

exports.setAnonymousMode = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const threadId = normalizeThreadId(request.data?.threadId);
  const enabled = Boolean(request.data?.enabled);
  const { thread } = await requireThreadMember(threadId, uid);
  if (thread.type !== "group" || thread.anonymousPolicy?.enabled === false) {
    throw new HttpsError("failed-precondition", "Anonymous mode is not enabled for this thread.");
  }

  const anonymousName = `匿名${pickInitialDisplayName(`${threadId}:${uid}`)}`;
  await db.collection("threads").doc(threadId).collection("members").doc(uid).set({
    anonymous: {
      enabled,
      name: anonymousName,
      seedVersion: 1,
      enabledAt: enabled ? FieldValue.serverTimestamp() : null
    }
  }, { merge: true });

  return { enabled, name: anonymousName };
});

exports.sendMessage = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const threadId = normalizeThreadId(request.data?.threadId);
  const text = normalizeMessageText(request.data?.text);
  const image = normalizeImageAttachment(request.data?.image, threadId);
  if (!text && !image) {
    throw new HttpsError("invalid-argument", "Message body is empty.");
  }

  const profile = await ensureActiveUser(uid);
  const { thread, member } = await requireThreadMember(threadId, uid);
  if (image) {
    await verifyImageOwner(image, uid, threadId);
  }
  const senderMode = member.anonymous?.enabled ? "anonymous" : "normal";
  const senderDisplayName = senderMode === "anonymous"
    ? member.anonymous.name
    : profile.displayName;
  const publicSenderUid = senderMode === "anonymous" ? null : uid;
  const messageRef = db.collection("threads").doc(threadId).collection("messages").doc();
  const auditRef = db.collection("threads").doc(threadId).collection("messageAudits").doc(messageRef.id);
  const now = FieldValue.serverTimestamp();
  const type = image ? "image" : "text";
  const lastMessage = {
    messageId: messageRef.id,
    type,
    text: text || (image ? "[image]" : ""),
    senderUid: publicSenderUid,
    senderDisplayName,
    createdAt: now
  };

  const memberRefs = await db.collection("threads").doc(threadId).collection("members").where("state", "==", "active").get();
  const batch = db.batch();
  batch.set(messageRef, {
    senderUid: publicSenderUid,
    senderMode,
    senderDisplayName,
    type,
    text,
    attachments: image ? [{
      storagePath: image.storagePath,
      downloadURL: image.downloadURL,
      contentType: image.contentType,
      size: image.size,
      moderationStatus: "pending"
    }] : [],
    createdAt: now,
    deletedAt: null
  });
  batch.set(auditRef, {
    senderUid: uid,
    senderMode,
    attachmentPaths: image ? [image.storagePath] : [],
    createdAt: now
  });
  batch.set(db.collection("threads").doc(threadId), {
    lastMessage,
    lastMessageAt: now
  }, { merge: true });

  for (const memberDoc of memberRefs.docs) {
    const memberUid = memberDoc.id;
    batch.set(sessionRef(memberUid, threadId), sessionData({
      threadId,
      type: thread.type,
      title: sessionTitleForMember(thread),
      avatar: null,
      lastMessage,
      lastMessageAt: now,
      unreadCount: memberUid === uid ? 0 : FieldValue.increment(1)
    }), { merge: true });
  }
  await batch.commit();

  return {
    threadId,
    messageId: messageRef.id
  };
});

exports.registerNotificationToken = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  await ensureActiveUser(uid);
  const token = normalizeNotificationToken(request.data?.token);
  const tokenHash = hashToken(token);

  await db.collection("users").doc(uid).collection("notificationTokens").doc(tokenHash).set({
    token,
    platform: String(request.data?.platform || "web").slice(0, 80),
    permission: String(request.data?.permission || "granted").slice(0, 32),
    userAgent: String(request.data?.userAgent || "").slice(0, 240),
    createdAt: FieldValue.serverTimestamp(),
    lastSeenAt: FieldValue.serverTimestamp(),
    disabledAt: null,
    lastError: null
  }, { merge: true });

  await db.collection("users").doc(uid).set({
    notificationSettings: {
      enabled: true,
      preview: false
    },
    updatedAt: FieldValue.serverTimestamp()
  }, { merge: true });

  return { tokenHash };
});

exports.unregisterNotificationToken = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const token = normalizeNotificationToken(request.data?.token);
  const tokenHash = hashToken(token);

  await db.collection("users").doc(uid).collection("notificationTokens").doc(tokenHash).set({
    disabledAt: FieldValue.serverTimestamp()
  }, { merge: true });

  return { tokenHash };
});

exports.markThreadRead = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  const threadId = normalizeThreadId(request.data?.threadId);
  await requireThreadMember(threadId, uid);
  const now = FieldValue.serverTimestamp();

  await db.runTransaction(async (transaction) => {
    transaction.set(db.collection("threads").doc(threadId).collection("members").doc(uid), {
      lastReadAt: now
    }, { merge: true });
    transaction.set(sessionRef(uid, threadId), {
      unreadCount: 0,
      updatedAt: now
    }, { merge: true });
  });

  return { threadId };
});

exports.deleteAccount = onCall(callableOptions, async (request) => {
  const uid = requireUid(request);
  requireRecentAuth(request);
  const jobRef = db.collection("deletionJobs").doc();
  await jobRef.set({
    uid,
    status: "running",
    startedAt: FieldValue.serverTimestamp(),
    finishedAt: null,
    error: null,
    counts: {}
  });

  try {
    const counts = await deleteUserData(uid);
    await admin.auth().deleteUser(uid);
    await jobRef.set({
      status: "complete",
      finishedAt: FieldValue.serverTimestamp(),
      counts
    }, { merge: true });
    return { jobId: jobRef.id, counts };
  } catch (error) {
    await jobRef.set({
      status: "failed",
      finishedAt: FieldValue.serverTimestamp(),
      error: String(error.message || error)
    }, { merge: true });
    throw error;
  }
});

exports.notifyThreadMembers = onDocumentCreated({
  region: REGION,
  document: "threads/{threadId}/messages/{messageId}"
}, async (event) => {
  const message = event.data?.data();
  if (!message || message.deletedAt) {
    return;
  }

  const threadId = event.params.threadId;
  const audit = await db.collection("threads").doc(threadId).collection("messageAudits").doc(event.params.messageId).get();
  const senderUid = audit.data()?.senderUid || message.senderUid;
  const memberSnapshot = await db.collection("threads").doc(threadId).collection("members").where("state", "==", "active").get();
  const recipientUids = memberSnapshot.docs.map((doc) => doc.id).filter((uid) => uid !== senderUid);
  if (recipientUids.length === 0) {
    return;
  }

  const tokenDocs = [];
  for (const uid of recipientUids) {
    const user = await db.collection("users").doc(uid).get();
    if (!user.exists || user.data().status !== "active" || user.data().notificationSettings?.enabled !== true) {
      continue;
    }
    const tokens = await db.collection("users").doc(uid).collection("notificationTokens").where("disabledAt", "==", null).get();
    for (const tokenDoc of tokens.docs) {
      tokenDocs.push({ uid, tokenHash: tokenDoc.id, token: tokenDoc.data().token });
    }
  }

  if (tokenDocs.length === 0) {
    return;
  }

  const response = await admin.messaging().sendEachForMulticast({
    tokens: tokenDocs.map((item) => item.token),
    notification: {
      title: "Eva-CLI",
      body: "You have a new chat message."
    },
    data: {
      type: "chat_message",
      threadId,
      messageId: event.params.messageId
    },
    webpush: {
      fcmOptions: {
        link: `${SITE_ORIGIN}/#discussion&thread=${encodeURIComponent(threadId)}`
      }
    }
  });

  const cleanup = [];
  response.responses.forEach((result, index) => {
    if (result.success) {
      return;
    }
    const tokenDoc = tokenDocs[index];
    cleanup.push(db.collection("users").doc(tokenDoc.uid).collection("notificationTokens").doc(tokenDoc.tokenHash).set({
      disabledAt: FieldValue.serverTimestamp(),
      lastError: result.error?.code || "unknown"
    }, { merge: true }));
  });
  await Promise.all(cleanup);
});

function requireUid(request) {
  const uid = request.auth?.uid;
  if (!uid) {
    throw new HttpsError("unauthenticated", "Sign in is required.");
  }
  return uid;
}

function requireRecentAuth(request) {
  const authTimeSeconds = Number(request.auth?.token?.auth_time || 0);
  if (!authTimeSeconds || Date.now() / 1000 - authTimeSeconds > 5 * 60) {
    throw new HttpsError("failed-precondition", "Recent sign-in is required before account deletion.");
  }
}

async function ensureActiveUser(uid) {
  const snapshot = await db.collection("users").doc(uid).get();
  if (!snapshot.exists || snapshot.data().status !== "active") {
    throw new HttpsError("failed-precondition", "Active user profile is required.");
  }
  return snapshot.data();
}

async function requireThreadMember(threadId, uid) {
  const [threadSnapshot, memberSnapshot] = await Promise.all([
    db.collection("threads").doc(threadId).get(),
    db.collection("threads").doc(threadId).collection("members").doc(uid).get()
  ]);
  if (!threadSnapshot.exists) {
    throw new HttpsError("not-found", "Thread not found.");
  }
  if (!memberSnapshot.exists || memberSnapshot.data().state !== "active") {
    throw new HttpsError("permission-denied", "Thread membership is required.");
  }
  return {
    thread: threadSnapshot.data(),
    member: memberSnapshot.data()
  };
}

function memberData({ uid, role, threadType }) {
  return {
    uid,
    role,
    threadType,
    joinedAt: FieldValue.serverTimestamp(),
    lastReadAt: null,
    state: "active",
    anonymous: {
      enabled: false,
      name: null,
      seedVersion: 1,
      enabledAt: null
    }
  };
}

function sessionRef(uid, threadId) {
  return db.collection("users").doc(uid).collection("sessions").doc(threadId);
}

function sessionData({ threadId, type, title, avatar, lastMessage, lastMessageAt, unreadCount, includeDefaults = false }) {
  const data = {
    threadId,
    type,
    lastMessage: lastMessage || null,
    lastMessageAt: lastMessageAt || FieldValue.serverTimestamp(),
    unreadCount,
    updatedAt: FieldValue.serverTimestamp()
  };

  if (includeDefaults) {
    data.muted = false;
    data.pinned = false;
  }

  if (title !== undefined && title !== null) {
    data.title = title || threadId;
  }
  if (avatar !== undefined) {
    data.avatar = avatar || null;
  }

  return data;
}

function sessionTitleForMember(thread) {
  if (thread.type === "group") {
    return thread.title || "Group";
  }
  return null;
}

function directThreadId(uidA, uidB) {
  const [a, b] = [uidA, uidB].sort();
  return `direct_${crypto.createHash("sha256").update(`${a}:${b}`).digest("hex")}`;
}

function hashToken(token) {
  return crypto.createHash("sha256").update(token).digest("hex");
}

function pickInitialDisplayName(seed) {
  const hash = crypto.createHash("sha256").update(seed).digest();
  return CHINESE_NAMES[hash[0] % CHINESE_NAMES.length];
}

function normalizeDisplayName(value) {
  const displayName = String(value || "").trim().replace(/\s+/g, " ");
  if (displayName.length < 2 || displayName.length > MAX_DISPLAY_NAME_LENGTH) {
    throw new HttpsError("invalid-argument", `Display name must be 2-${MAX_DISPLAY_NAME_LENGTH} characters.`);
  }
  return displayName;
}

function normalizeTitle(value) {
  const title = String(value || "").trim().replace(/\s+/g, " ");
  if (title.length < 2 || title.length > MAX_GROUP_TITLE_LENGTH) {
    throw new HttpsError("invalid-argument", `Group title must be 2-${MAX_GROUP_TITLE_LENGTH} characters.`);
  }
  return title;
}

function normalizeMessageText(value) {
  const text = String(value || "").trim().replace(/\s+/g, " ");
  if (text.length > MAX_TEXT_LENGTH) {
    throw new HttpsError("invalid-argument", `Message text cannot exceed ${MAX_TEXT_LENGTH} characters.`);
  }
  return text;
}

function normalizeUid(value) {
  const uid = String(value || "").trim();
  if (!/^[A-Za-z0-9:_-]{6,128}$/.test(uid)) {
    throw new HttpsError("invalid-argument", "Invalid user UID.");
  }
  return uid;
}

function normalizeThreadId(value) {
  const threadId = String(value || "").trim();
  if (!/^[A-Za-z0-9:_-]{6,160}$/.test(threadId)) {
    throw new HttpsError("invalid-argument", "Invalid thread ID.");
  }
  return threadId;
}

function normalizeToken(value) {
  const token = String(value || "").trim();
  if (!/^[A-Za-z0-9_-]{32,256}$/.test(token)) {
    throw new HttpsError("invalid-argument", "Invalid invite token.");
  }
  return token;
}

function normalizeNotificationToken(value) {
  const token = String(value || "").trim();
  if (token.length < 32 || token.length > 4096) {
    throw new HttpsError("invalid-argument", "Invalid notification token.");
  }
  return token;
}

function normalizeImageAttachment(value, threadId) {
  if (!value) {
    return null;
  }
  const storagePath = String(value.storagePath || "");
  const downloadURL = String(value.downloadURL || "");
  const contentType = String(value.contentType || "");
  const size = Number(value.size || 0);
  if (!storagePath.startsWith(`chat/${threadId}/`) || !downloadURL.startsWith("https://") || !contentType.startsWith("image/")) {
    throw new HttpsError("invalid-argument", "Invalid image attachment.");
  }
  if (!Number.isFinite(size) || size <= 0 || size > MAX_IMAGE_BYTES) {
    throw new HttpsError("invalid-argument", "Invalid image size.");
  }
  return {
    messageId: String(value.messageId || "").slice(0, 80),
    storagePath,
    downloadURL,
    contentType,
    size
  };
}

async function verifyImageOwner(image, uid, threadId) {
  const [metadata] = await bucket.file(image.storagePath).getMetadata();
  const customMetadata = metadata.metadata || {};
  if (customMetadata.ownerUid !== uid || customMetadata.threadId !== threadId) {
    throw new HttpsError("permission-denied", "Image attachment does not belong to this user and thread.");
  }
  if (!String(metadata.contentType || "").startsWith("image/")) {
    throw new HttpsError("invalid-argument", "Stored attachment is not an image.");
  }
  if (Number(metadata.size || 0) > MAX_IMAGE_BYTES) {
    throw new HttpsError("invalid-argument", "Stored image is too large.");
  }
}

function clampInteger(value, min, max, fallback) {
  const number = Number(value);
  if (!Number.isInteger(number)) {
    return fallback;
  }
  return Math.min(max, Math.max(min, number));
}

function serializeSnapshot(snapshot) {
  const data = snapshot.data();
  return {
    uid: snapshot.id,
    ...data,
    createdAt: data.createdAt?.toMillis?.() || null,
    updatedAt: data.updatedAt?.toMillis?.() || null
  };
}

async function deleteUserData(uid) {
  const counts = {
    userDocs: 0,
    sessions: 0,
    memberships: 0,
    messages: 0,
    files: 0
  };

  const userRef = db.collection("users").doc(uid);
  await deleteCollection(userRef.collection("notificationTokens"), 100);
  counts.sessions += await deleteCollection(userRef.collection("sessions"), 100);
  await userRef.delete();
  counts.userDocs += 1;

  const threads = await db.collection("threads").get();
  for (const thread of threads.docs) {
    const threadData = thread.data();
    const memberRef = thread.ref.collection("members").doc(uid);
    const member = await memberRef.get();
    if (member.exists) {
      if (threadData.type === "direct") {
        const deletedThreadCounts = await deleteDirectThread(thread);
        counts.sessions += deletedThreadCounts.sessions;
        counts.memberships += deletedThreadCounts.memberships;
        counts.messages += deletedThreadCounts.messages;
        counts.files += deletedThreadCounts.files;
        continue;
      }

      await memberRef.delete();
      await thread.ref.set({ memberCount: FieldValue.increment(-1) }, { merge: true });
      counts.memberships += 1;
    }

    const messageIds = new Set();
    const publicMessages = await thread.ref.collection("messages").where("senderUid", "==", uid).get();
    const auditMessages = await thread.ref.collection("messageAudits").where("senderUid", "==", uid).get();
    for (const message of publicMessages.docs) {
      messageIds.add(message.id);
    }
    for (const audit of auditMessages.docs) {
      messageIds.add(audit.id);
    }

    for (const messageId of messageIds) {
      const message = await thread.ref.collection("messages").doc(messageId).get();
      if (!message.exists) {
        await thread.ref.collection("messageAudits").doc(messageId).delete();
        continue;
      }
      const attachments = message.data().attachments || [];
      for (const attachment of attachments) {
        if (attachment.storagePath) {
          await bucket.file(attachment.storagePath).delete({ ignoreNotFound: true });
          counts.files += 1;
        }
      }
      await message.ref.delete();
      await thread.ref.collection("messageAudits").doc(messageId).delete();
      counts.messages += 1;
    }

    if (messageIds.has(threadData.lastMessage?.messageId)) {
      await clearLastMessageProjection(thread);
    }
  }

  await rtdb.ref(`status/${uid}`).remove();
  return counts;
}

async function deleteDirectThread(threadSnapshot) {
  const counts = {
    sessions: 0,
    memberships: 0,
    messages: 0,
    files: 0
  };

  const members = await threadSnapshot.ref.collection("members").get();
  const messages = await threadSnapshot.ref.collection("messages").get();
  const audits = await threadSnapshot.ref.collection("messageAudits").get();

  for (const message of messages.docs) {
    const attachments = message.data().attachments || [];
    for (const attachment of attachments) {
      if (attachment.storagePath) {
        await bucket.file(attachment.storagePath).delete({ ignoreNotFound: true });
        counts.files += 1;
      }
    }
    await message.ref.delete();
    counts.messages += 1;
  }

  for (const member of members.docs) {
    await sessionRef(member.id, threadSnapshot.id).delete();
    await member.ref.delete();
    counts.sessions += 1;
    counts.memberships += 1;
  }

  for (const audit of audits.docs) {
    await audit.ref.delete();
  }

  await threadSnapshot.ref.delete();
  return counts;
}

async function clearLastMessageProjection(threadSnapshot) {
  const members = await threadSnapshot.ref.collection("members").get();
  await threadSnapshot.ref.set({
    lastMessage: null
  }, { merge: true });

  const batch = db.batch();
  for (const member of members.docs) {
    batch.set(sessionRef(member.id, threadSnapshot.id), {
      lastMessage: null,
      updatedAt: FieldValue.serverTimestamp()
    }, { merge: true });
  }
  await batch.commit();
}

async function deleteCollection(collectionRef, batchSize) {
  let deleted = 0;
  while (true) {
    const snapshot = await collectionRef.limit(batchSize).get();
    if (snapshot.empty) {
      break;
    }
    const batch = db.batch();
    for (const doc of snapshot.docs) {
      batch.delete(doc.ref);
      deleted += 1;
    }
    await batch.commit();
  }
  return deleted;
}
