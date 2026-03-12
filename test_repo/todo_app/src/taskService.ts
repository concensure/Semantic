import {
  addTag,
  completeTask,
  createTask,
  filterByTag,
  listOverdueTasks,
  listTasks,
  removeTag,
  reorderPriority,
  setDueDate,
  setPriority,
} from "./taskStore";
import { Task, TaskPriority } from "./types";

export function addTask(title: string, priority: TaskPriority = "MEDIUM"): Task {
  return createTask({
    id: `task-${Math.random().toString(16).slice(2)}`,
    title,
    priority,
  });
}

export function finishTask(id: string): Task | undefined {
  return completeTask(id);
}

export function updatePriority(id: string, priority: TaskPriority): Task | undefined {
  return setPriority(id, priority);
}

export function updateDueDate(id: string, dueDate: string): Task | undefined {
  return setDueDate(id, dueDate);
}

export function attachTag(id: string, tag: string): Task | undefined {
  return addTag(id, tag);
}

export function detachTag(id: string, tag: string): Task | undefined {
  return removeTag(id, tag);
}

export function getTasksByTag(tag: string): Task[] {
  return filterByTag(tag);
}

export function getOrderedTasks(): Task[] {
  return reorderPriority();
}

export function getOverdueTasks(nowIso?: string): Task[] {
  return listOverdueTasks(nowIso);
}

export function allTasks(): Task[] {
  return listTasks();
}
