import "./main.css";
import { initApp } from "./app.js";

const root = document.getElementById("app");
if (!root) throw new Error("Missing #app");
initApp(root);
