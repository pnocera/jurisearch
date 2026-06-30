import { type ClassValue, clsx } from "clsx";
import { twMerge } from "tailwind-merge";

/** The shadcn-vue class merge helper: conditional classes (clsx) + Tailwind conflict resolution. */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs));
}
