import { TaskMenu, listToolActions } from "./menu";
import { allTasks } from "./taskService";

export function renderAppHome(): string {
  const taskCount = allTasks().length;
  return `Tasks(${taskCount}) :: ${TaskMenu()}`;
}

export function getToolsPanel(): string[] {
  return listToolActions().map((a) => `${a.kind}:${a.label}`);
}
