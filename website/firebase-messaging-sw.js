importScripts("https://www.gstatic.com/firebasejs/12.15.0/firebase-app-compat.js");
importScripts("https://www.gstatic.com/firebasejs/12.15.0/firebase-messaging-compat.js");

firebase.initializeApp({
  apiKey: "AIzaSyCeFl2liIETM6t5L9wVRA89b922UGGtzls",
  authDomain: "eva-cli-7ad30.firebaseapp.com",
  projectId: "eva-cli-7ad30",
  storageBucket: "eva-cli-7ad30.firebasestorage.app",
  messagingSenderId: "83967779785",
  appId: "1:83967779785:web:6d27fdac0cff0e71b2be18",
  measurementId: "G-08QR0NPT78"
});

const messaging = firebase.messaging();

messaging.onBackgroundMessage((payload) => {
  const notificationTitle = payload.notification?.title || "Eva-CLI";
  const notificationOptions = {
    body: payload.notification?.body || "You have a new chat message.",
    data: payload.data || {},
    icon: "/assets/eva-cli-logo.svg",
    badge: "/assets/eva-cli-logo.svg"
  };

  self.registration.showNotification(notificationTitle, notificationOptions);
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const threadId = event.notification.data?.threadId;
  const targetUrl = threadId ? `/#discussion&thread=${encodeURIComponent(threadId)}` : "/#discussion";

  event.waitUntil(
    clients.matchAll({ type: "window", includeUncontrolled: true }).then((clientList) => {
      for (const client of clientList) {
        if ("focus" in client) {
          client.navigate(targetUrl);
          return client.focus();
        }
      }

      return clients.openWindow(targetUrl);
    })
  );
});
