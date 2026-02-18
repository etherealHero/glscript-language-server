// This script is included via DEFAULT_INCLUDED.js.
// Therefore, these functions are global and visible everywhere.

/** @param {string} message - The text to log. */
function logInfo(message) {
  console.log("[INFO]: " + message);
}

/**
 * @param {number} a
 * @returns {number}
 */
function getSquare(a) {
  return a * a;
}
