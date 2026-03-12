import { Task, TaskPriority } from "./types";

const tasks: Task[] = [];

export function createTask(input: {
  id: string;
  title: string;
  priority?: TaskPriority;
  dueDate?: string;
  tags?: string[];
}): Task {
  const task: Task = {
    id: input.id,
    title: input.title.trim(),
    completed: false,
    priority: input.priority ?? "MEDIUM",
    dueDate: input.dueDate,
    tags: input.tags ?? [],
    createdAt: new Date().toISOString(),
  };
  tasks.push(task);
  return task;
}

export function listTasks(): Task[] {
  return [...tasks];
}

export function completeTask(id: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.completed = true;
  return task;
}

export function setDueDate(id: string, dueDate: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.dueDate = dueDate;
  return task;
}

export function setPriority(id: string, priority: TaskPriority): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.priority = priority;
  return task;
}

export function reorderPriority(): Task[] {
  return [...tasks].sort((a, b) => a.priority.localeCompare(b.priority));
}

export function addTag(id: string, tag: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.tags.push(tag);
  return task;
}

export function removeTag(id: string, tag: string): Task | undefined {
  const task = tasks.find((t) => t.id === id);
  if (!task) return undefined;
  task.tags = task.tags.filter((t) => t !== tag);
  return task;
}

export function filterByTag(tag: string): Task[] {
  return tasks.filter((t) => t.tags.includes(tag));
}

export function listOverdueTasks(nowIso: string = new Date().toISOString()): Task[] {
  const now = Date.parse(nowIso);
  return tasks.filter((t) => !!t.dueDate && Date.parse(t.dueDate!) < now);
}
