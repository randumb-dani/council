import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

// The PWA shell registers only in a real browser: inside Tauri there is no
// service-worker scope worth having, and in `npm run dev` without the local
// server there is nothing to stay fresh for.
if ("__TAURI_INTERNALS__" in window === false && "serviceWorker" in navigator) {
  window.addEventListener("load", () => {
    navigator.serviceWorker.register("/sw.js").catch(() => {
      /* private mode or no scope: the app runs fine without the shell cache */
    });
  });
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
