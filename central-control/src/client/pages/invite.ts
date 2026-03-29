import * as api from "../api.js";
import { state, showBanner } from "../state.js";
import { navigate } from "../router.js";
import { esc } from "../utils.js";

export function renderInvitePage(token: string): string {
  return `
    <div class="min-h-screen flex items-center justify-center">
      <div class="bg-gray-900 border border-gray-800 rounded-2xl p-8 w-full max-w-md shadow-2xl">
        <h1 class="text-2xl font-bold mb-4 text-center">Invitation</h1>
        <div id="invite-content" class="text-center">
          <p class="text-gray-400">Loading invitation details...</p>
        </div>
      </div>
    </div>`;
}

export async function bindInviteEvents(root: HTMLElement, token: string): Promise<void> {
  const content = root.querySelector("#invite-content");
  if (!content) return;

  if (!state.user) {
    content.innerHTML = `
      <p class="text-gray-400 mb-4">You need to log in or sign up to respond to this invitation.</p>
      <div class="flex gap-3 justify-center">
        <a href="#/login" class="bg-blue-600 hover:bg-blue-700 text-white font-semibold rounded-lg px-4 py-3 transition-colors">Log In</a>
        <a href="#/signup" class="bg-gray-700 hover:bg-gray-600 text-white font-semibold rounded-lg px-4 py-3 transition-colors">Sign Up</a>
      </div>`;
    return;
  }

  try {
    const invitation = await api.getInvitation(token);

    if (invitation.status !== "pending") {
      content.innerHTML = `<p class="text-gray-400">This invitation has already been ${esc(invitation.status)}.</p>
        <a href="#/" class="inline-block mt-4 text-blue-400 hover:underline">Go to Dashboard</a>`;
      return;
    }

    content.innerHTML = `
      <p class="text-gray-300 mb-2"><strong>${esc(invitation.fromLogin)}</strong> has invited you to control their collar devices.</p>
      <p class="text-sm text-gray-500 mb-6">The device owner will set specific access permissions after you accept.</p>
      <div class="flex gap-3 justify-center">
        <button id="accept-invite" class="bg-green-600 hover:bg-green-700 text-white font-semibold rounded-lg px-6 py-3 transition-colors">Accept</button>
        <button id="reject-invite" class="bg-red-600 hover:bg-red-700 text-white font-semibold rounded-lg px-6 py-3 transition-colors">Reject</button>
      </div>`;

    root.querySelector("#accept-invite")?.addEventListener("click", async () => {
      try {
        await api.acceptInvitation(token);
        showBanner("info", "Invitation accepted");
        navigate("/");
      } catch (err) {
        showBanner("error", err instanceof Error ? err.message : "Failed to accept");
      }
    });

    root.querySelector("#reject-invite")?.addEventListener("click", async () => {
      try {
        await api.rejectInvitation(token);
        showBanner("info", "Invitation rejected");
        navigate("/");
      } catch (err) {
        showBanner("error", err instanceof Error ? err.message : "Failed to reject");
      }
    });
  } catch (err) {
    content.innerHTML = `<p class="text-red-400">${esc(err instanceof Error ? err.message : "Failed to load invitation")}</p>
      <a href="#/" class="inline-block mt-4 text-blue-400 hover:underline">Go to Dashboard</a>`;
  }
}
