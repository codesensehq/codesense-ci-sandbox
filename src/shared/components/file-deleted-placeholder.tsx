// SPDX-License-Identifier: MIT
import { FileX } from "lucide-react";

interface FileDeletedPlaceholderProps {
  fileName: string;
  onClose: () => void;
}

export function FileDeletedPlaceholder({ fileName, onClose }: FileDeletedPlaceholderProps) {
  return (
    <div className="flex flex-col items-center justify-center h-full gap-3 text-muted-foreground">
      <FileX size={32} className="opacity-40" />
      <p className="text-sm font-medium">{fileName}</p>
      <p className="text-xs">This file was removed from disk</p>
      <button
        className="mt-2 px-3 py-1.5 rounded text-xs bg-muted hover:bg-muted/80 text-foreground transition-colors"
        onClick={onClose}
      >
        Close Tab
      </button>
    </div>
  );
}
