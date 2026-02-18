// Note: We did NOT include 'global_script.js' here.
// But 'logInfo' and 'APP_VERSION' are available because
// they are defined in the globally included DEFAULT_INCLUDED.

// Using the global variable from DEFAULT_INCLUDED.
// No import needed, it is injected automatically.
logInfo("Initializing Math Utils. Version: " + APP_VERSION);

/**
 * @param {number} radius
 * @returns {number}
 */
function getCircleArea(radius) {
  // Using the global helper from global_script.js.
  // It works because DEFAULT_INCLUDED imports global_script.
  return 3.14 * getSquare(radius);
}
