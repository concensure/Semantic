export type ToolAction = {
  id: string;
  label: string;
  kind: "due_date" | "tag";
};

const tools: ToolAction[] = [
  { id: "due-today", label: "Due Today", kind: "due_date" },
  { id: "tag-bug", label: "Tag: bug", kind: "tag" },
];

export function registerTool(action: ToolAction): ToolAction[] {
  tools.push(action);
  return [...tools];
}

export function listToolActions(): ToolAction[] {
  return [...tools];
}

export function TaskMenu(): string {
  return tools.map((t) => t.label).join(" | ");
}
