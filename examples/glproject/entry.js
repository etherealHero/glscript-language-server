// This is the entry point of the application.

// Explicitly include the math utilities.
// This inserts the content of math.js right here.
#include <./utils/math.js>
#include <utils/math.js> // absolute path supports too
import "utils/math.js" // import syntax supports too

let msg =
#text
Application started.
#endtext

// 'logInfo' comes from global_script.js (included globally).
// It is available here without any #include directive.

logInfo(msg);

// 'getCircleArea' comes from utils/math.js.
// It is available here because we included it above.
let area = getCircleArea(5);

// 'APP_VERSION' comes from DEFAULT_INCLUDED.
// It is available globally in every file.
logInfo("Circle area calculated: " + area);
logInfo("Current system version: " + APP_VERSION);
