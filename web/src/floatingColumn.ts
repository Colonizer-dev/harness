// Where the fixed card column (the storage alert and the live-map prompt) sits. It lives on its own
// so the rule can be pinned in a test without rendering App.
//
// The cockpit's inspector is a 360px aside flush with the right edge, and its open-colony and stop
// buttons sit at its foot, right where the column's bottom-right corner used to land (issue #196).
// While the inspector shows, the column moves left past it, keeping the same 20px gap.

/**
 * The column's classes: full width on a narrow window, else the bottom-right corner clear of any
 * inspector. A phone (below `sm`) runs the cockpit with the tab bar across the foot (issue #516),
 * so the desktop corner is overridden to full width above the bar — the same offset the composer keeps.
 */
export function floatingColumnClass(narrow: boolean, inspectorShown: boolean): string {
  if (narrow) return "inset-x-3 bottom-3";
  const phone = "max-sm:left-3 max-sm:right-3 max-sm:bottom-[calc(4.25rem+env(safe-area-inset-bottom))] max-sm:w-auto";
  return (inspectorShown ? "bottom-5 right-[380px] w-[380px]" : "bottom-5 right-5 w-[380px]") + " " + phone;
}
