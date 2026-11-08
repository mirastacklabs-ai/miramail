// Keyboard shortcut reference: shown in Preferences > Shortcuts and in the `?` cheat sheet.
export const SHORTCUTS: { group: string; rows: [string, string][] }[] = [
  { group: "Anywhere", rows: [["⌘ ,", "Open Preferences"], ["⌘ J", "Show / hide the AI chat pane"], ["?", "Show this cheat sheet"], ["⌘ /", "Show this cheat sheet"], ["Esc", "Close dialog, clear search"], ["g then m", "Go to Mail"], ["g then c", "Go to Calendar"], ["⌘ 1 / ⌘ 2", "Mail / Calendar"]] },
  { group: "Mail", rows: [["j / k", "Next / previous message (conversations count as one row)"], ["← / →", "Collapse / expand the selected conversation"], ["Enter", "Open selected message"], ["⌘ N or c", "Compose a new message"], ["⌘ R or r", "Reply"], ["⇧ ⌘ R", "Reply all"], ["⇧ ⌘ F", "Forward"], ["a or ⌃ ⌘ A", "Archive"], ["#", "Move to Trash"], ["⇧ ⌘ U / ⌥ ⌘ U", "Mark read / unread"], ["⇧ ⌘ L", "Flag / unflag"], ["⇧ ⌘ M", "Get new mail"], ["⌥ ⌘ 1 ... 6", "Inbox, Drafts, Sent, Archive, Trash, Junk"], ["/", "Focus search"]] },
  { group: "Panes and windows", rows: [["⌘ W", "Close the focused pane"], ["⌘ Q", "Quit MiraMail"], ["⇧ ⌘ O", "Move pane to its own window"], ["⇧ ⌘ A", "Move pane back to the main window"]] },
  { group: "AI chat", rows: [["⌘ Enter", "Send"], ["⌘ .", "Stop the reply"], ["Esc", "Leave the message box"]] },
  { group: "Composing", rows: [["⌘ Enter", "Send"], ["Esc", "Close the composer"]] },
  { group: "Calendar", rows: [["t", "Today"], ["n", "New event"], ["← / →", "Previous / next period"], ["1 2 3 4", "Day / Week / Month / Agenda"], ["Esc", "Close popover"]] },
];
