// tests/integration.js
console.log("[Test 1] Starting integration tests...");

// 1. Console Verification
console.log("[Test 1.1] Verify console.log works");
console.error("[Test 1.2] Verify console.error works");

// 2. Sync Filesystem Verification
console.log("[Test 2.1] Sync Write File");
const testFilename = "integration-test-temp.txt";
const expectedContent = "Runtime test data: " + Math.random();
fs.writeFileSync(testFilename, expectedContent);

console.log("[Test 2.2] Sync Read File");
const actualContent = fs.readFileSync(testFilename);
if (actualContent !== expectedContent) {
    throw new Error(`Sync filesystem verification failed: expected "${expectedContent}", got "${actualContent}"`);
}
console.log("[Test 2.3] Sync FS match verified successfully");

// 3. Timers & Async FS Verification (Concurrent)
console.log("[Test 3.1] Scheduling Timeout");
let timeoutFired = false;
let asyncFsFinished = false;

setTimeout(() => {
    timeoutFired = true;
    console.log("[Test 3.2] Timeout callback executed");
    checkSuccess();
}, 50);

console.log("[Test 4.1] Async Write File");
const asyncFilename = "integration-test-temp-async.txt";
const asyncExpectedContent = "Async runtime test data: " + Math.random();

fs.writeFile(asyncFilename, asyncExpectedContent)
    .then(() => {
        console.log("[Test 4.2] Async write finished, initiating read...");
        return fs.readFile(asyncFilename);
    })
    .then(actualAsyncContent => {
        console.log("[Test 4.3] Async read finished");
        if (actualAsyncContent !== asyncExpectedContent) {
            throw new Error(`Async filesystem verification failed: expected "${asyncExpectedContent}", got "${actualAsyncContent}"`);
        }
        console.log("[Test 4.4] Async FS match verified successfully");
        asyncFsFinished = true;
        checkSuccess();
    })
    .catch(err => {
        console.error("Test failed with error:", err);
        throw err;
    });

function checkSuccess() {
    if (timeoutFired && asyncFsFinished) {
        console.log("\n=============================");
        console.log("===  ALL TESTS PASSED!    ===");
        console.log("=============================\n");
    }
}

console.log("[Test 3.3] Main script execution finished, waiting for async queue...");
