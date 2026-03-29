import * as api from "../api.js";
import { state, showBanner, triggerRender } from "../state.js";
import { navigate } from "../router.js";
import * as ws from "../ws.js";
import { esc } from "../utils.js";

export function renderLoginPage(): string {
  return `
    <div class="min-h-screen flex items-center justify-center">
      <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-md shadow-2xl">
        <h1 class="text-2xl font-bold mb-6 text-center">Rusty Collars Central Control</h1>
        <form id="login-form" class="flex flex-col gap-4">
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Login</span>
            <input type="text" name="login" required autocomplete="username"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Password</span>
            <input type="password" name="password" required autocomplete="current-password"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
          </label>
          <button type="submit"
            class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">
            Log In
          </button>
        </form>
        <p class="text-center text-sm text-gray-500 mt-4">
          No account? <a href="#/signup" class="text-blue-400 hover:underline">Sign up</a>
        </p>
      </div>
    </div>`;
}

export function renderSignupPage(): string {
  return `
    <div class="min-h-screen flex items-center justify-center">
      <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-md shadow-2xl">
        <h1 class="text-2xl font-bold mb-6 text-center">Create Account</h1>
        <form id="signup-form" class="flex flex-col gap-4">
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Login</span>
            <input type="text" name="login" required minlength="3" autocomplete="username"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Password</span>
            <input type="password" name="password" required minlength="8" autocomplete="new-password"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
          </label>
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Confirm Password</span>
            <input type="password" name="confirm" required minlength="8" autocomplete="new-password"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 focus:outline-none focus:border-blue-500">
          </label>
          <button type="submit"
            class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">
            Sign Up
          </button>
        </form>
        <p class="text-center text-sm text-gray-500 mt-4">
          Already have an account? <a href="#/login" class="text-blue-400 hover:underline">Log in</a>
        </p>
      </div>
    </div>`;
}

export function renderTotpLoginPage(): string {
  return `
    <div class="min-h-screen flex items-center justify-center">
      <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-md shadow-2xl">
        <h1 class="text-2xl font-bold mb-6 text-center">Two-Factor Authentication</h1>
        <form id="totp-login-form" class="flex flex-col gap-4">
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Enter the 6-digit code from your authenticator app</span>
            <input type="text" name="code" required pattern="[0-9]{6}" maxlength="6" autocomplete="one-time-code"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 text-center text-2xl tracking-widest font-mono focus:outline-none focus:border-blue-500">
          </label>
          <button type="submit"
            class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">
            Verify
          </button>
        </form>
      </div>
    </div>`;
}

export function renderTotpSetupPage(setupData: { secret: string; qrDataUrl: string } | null): string {
  if (!setupData) {
    return `
      <div class="min-h-screen flex items-center justify-center">
        <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-md shadow-2xl">
          <h1 class="text-2xl font-bold mb-6 text-center">Set Up Two-Factor Authentication</h1>
          <p class="text-gray-400 mb-4 text-center">Loading...</p>
          <button id="totp-setup-init"
            class="w-full bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">
            Generate 2FA Secret
          </button>
          <p class="text-center text-sm text-gray-500 mt-4">
            <a href="#/" class="text-blue-400 hover:underline">Skip for now</a>
          </p>
        </div>
      </div>`;
  }

  return `
    <div class="min-h-screen flex items-center justify-center">
      <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-lg shadow-2xl">
        <h1 class="text-2xl font-bold mb-6 text-center">Set Up Two-Factor Authentication</h1>
        <div class="flex flex-col items-center gap-4 mb-6">
          <img src="${esc(setupData.qrDataUrl)}" alt="QR Code" class="w-48 h-48 rounded-lg bg-white p-2">
          <div class="text-center">
            <p class="text-sm text-gray-400 mb-1">Or enter this code manually:</p>
            <code class="text-sm bg-gray-800 px-3 py-1.5 rounded-lg font-mono select-all">${esc(setupData.secret)}</code>
          </div>
        </div>
        <form id="totp-verify-form" class="flex flex-col gap-4">
          <label class="flex flex-col gap-1">
            <span class="text-sm text-gray-400">Enter verification code</span>
            <input type="text" name="code" required pattern="[0-9]{6}" maxlength="6" autocomplete="one-time-code"
              class="bg-gray-800 border border-gray-700 rounded-lg px-4 py-3 text-gray-100 text-center text-2xl tracking-widest font-mono focus:outline-none focus:border-blue-500">
          </label>
          <button type="submit"
            class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">
            Verify & Enable 2FA
          </button>
        </form>
        <p class="text-center text-sm text-gray-500 mt-4">
          <a href="#/" class="text-blue-400 hover:underline">Skip for now</a>
        </p>
      </div>
    </div>`;
}

// ── Event binding ──

export function bindLoginEvents(root: HTMLElement): void {
  const form = root.querySelector("#login-form") as HTMLFormElement | null;
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(form);
    const login = (formData.get("login") as string).trim();
    const password = formData.get("password") as string;

    try {
      const result = await api.login(login, password);
      if (result.requiresTotp && result.pendingToken) {
        (window as any).__pendingTotpToken = result.pendingToken;
        navigate("/totp-login");
        return;
      }
      if (result.sessionToken && result.user) {
        state.sessionToken = result.sessionToken;
        state.user = result.user;
        ws.connect();
        navigate("/");
      }
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Login failed");
    }
  });
}

export function bindSignupEvents(root: HTMLElement): void {
  const form = root.querySelector("#signup-form") as HTMLFormElement | null;
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(form);
    const login = (formData.get("login") as string).trim();
    const password = formData.get("password") as string;
    const confirm = formData.get("confirm") as string;

    if (password !== confirm) {
      showBanner("error", "Passwords do not match");
      return;
    }

    try {
      const result = await api.signup(login, password);
      if (result.sessionToken && result.user) {
        state.sessionToken = result.sessionToken;
        state.user = result.user;
        ws.connect();
        navigate("/setup-2fa");
      }
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Signup failed");
    }
  });
}

export function bindTotpLoginEvents(root: HTMLElement): void {
  const form = root.querySelector("#totp-login-form") as HTMLFormElement | null;
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(form);
    const code = (formData.get("code") as string).trim();
    const pendingToken = (window as any).__pendingTotpToken as string | undefined;

    if (!pendingToken) {
      showBanner("error", "Session expired, please log in again");
      navigate("/login");
      return;
    }

    try {
      const result = await api.validateTotp(pendingToken, code);
      delete (window as any).__pendingTotpToken;
      state.sessionToken = result.sessionToken;
      state.user = result.user;
      ws.connect();
      navigate("/");
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Invalid code");
    }
  });
}

let totpSetupData: { secret: string; qrDataUrl: string } | null = null;

export function getTotpSetupData(): typeof totpSetupData {
  return totpSetupData;
}

export function bindTotpSetupEvents(root: HTMLElement): void {
  const initBtn = root.querySelector("#totp-setup-init") as HTMLButtonElement | null;
  initBtn?.addEventListener("click", async () => {
    try {
      const setup = await api.setupTotp();
      totpSetupData = { secret: setup.secret, qrDataUrl: setup.qrDataUrl };
      triggerRender();
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Failed to generate 2FA secret");
    }
  });

  const form = root.querySelector("#totp-verify-form") as HTMLFormElement | null;
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const formData = new FormData(form);
    const code = (formData.get("code") as string).trim();

    try {
      await api.verifyTotp(code);
      totpSetupData = null;
      showBanner("info", "2FA enabled successfully");
      if (state.user) state.user.totpEnabled = true;
      navigate("/");
    } catch (err) {
      showBanner("error", err instanceof Error ? err.message : "Invalid code");
    }
  });
}
