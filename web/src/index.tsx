import { render } from "solid-js/web";

import App from "./App";
import "./styles.css";

const root = document.getElementById("root");
if (!root) throw new Error("root element not found");
const appRoot = root;

window.addEventListener("error", (event) => {
  if (event.message.startsWith("ResizeObserver loop")) {
    event.preventDefault();
    return;
  }
  showFatal(event.error ?? event.message);
});
window.addEventListener("unhandledrejection", (event) =>
  showFatal(event.reason),
);

try {
  render(() => <App />, appRoot);
} catch (error) {
  showFatal(error);
}

function showFatal(reason: unknown): void {
  const message = reason instanceof Error ? reason.message : String(reason);
  appRoot.replaceChildren();
  const main = document.createElement("main");
  main.className = "startup";
  const card = document.createElement("div");
  card.className = "startup-card";
  const title = document.createElement("h1");
  title.textContent = "Panestraを起動できませんでした";
  const detail = document.createElement("p");
  detail.textContent = message;
  card.append(title, detail);
  main.append(card);
  appRoot.append(main);
}
