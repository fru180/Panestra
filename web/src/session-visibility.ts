import type { Session } from "./types";

export function isVisibleTerminal(
  session: Pick<Session, "processState">,
): boolean {
  return (
    session.processState === "starting" || session.processState === "running"
  );
}
