import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./index.css";

const Root = import.meta.env.DEV ? (
  <React.StrictMode>
    <App />
  </React.StrictMode>
) : (
  <App />
);

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(Root);
