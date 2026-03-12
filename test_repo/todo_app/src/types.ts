export type TaskPriority = "HIGH" | "MEDIUM" | "LOW";

export interface Task {
  id: string;
  title: string;
  completed: boolean;
  priority: TaskPriority;
  dueDate?: string;
  tags: string[];
  createdAt: string;
}
