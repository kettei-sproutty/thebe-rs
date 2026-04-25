const path = require("node:path");
const Mocha = require("mocha");

async function run() {
  const mocha = new Mocha({
    ui: "tdd",
    color: true,
    timeout: 120000,
  });

  mocha.addFile(path.resolve(__dirname, "commands.test.js"));

  return new Promise((resolve, reject) => {
    mocha.run((failures) => {
      if (failures > 0) {
        reject(new Error(`${failures} extension test(s) failed.`));
        return;
      }

      resolve();
    });
  });
}

module.exports = {
  run,
};
