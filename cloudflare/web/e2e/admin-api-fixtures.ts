export const adminUser = "operator";

export function adminMe(loggedIn = true) {
  return loggedIn
    ? { loggedIn: true, username: adminUser }
    : { loggedIn: false };
}

export const loginSuccess = { ok: true, username: adminUser };
export const logoutSuccess = { ok: true };

export function legacyError(message: string) {
  return { error: message };
}
