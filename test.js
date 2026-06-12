console.log("--- Sync Filesystem Test ---");
fs.writeFileSync("sync-test.txt", "Hello from synchronous file writing!");
const syncContents = fs.readFileSync("sync-test.txt");
console.log("Read Sync contents:", syncContents);

console.log("\n--- Async Filesystem Test ---");
fs.writeFile("async-test.txt", "Hello from asynchronous file writing!")
    .then(() => {
        console.log("Async file write finished.");
        return fs.readFile("async-test.txt");
    })
    .then(asyncContents => {
        console.log("Read Async contents:", asyncContents);
    })
    .catch(err => {
        console.error("Error during async file operation:", err);
    });

console.log("Main script finished executing.");
