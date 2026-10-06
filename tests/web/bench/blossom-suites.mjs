// The Blossom TodoMVC as Speedometer 3 suites (appended to Speedometer's suites/default-suites.mjs by
// scripts/bench-todomvc.sh). The steps are Speedometer's own TodoMVC steps in the variant its React, Vue, Preact,
// Svelte, Angular and Lit suites use (`input`, then Enter as `keydown`): the events the browser host listens to.
// `?bench` runs the app alone and saves nothing; `?bench=persist` saves its durable tables to localStorage after every
// event, as the app does outside a benchmark.
function blossomSuite(name, url) {
    return {
        name,
        url,
        tags: ["todomvc", "blossom"],
        async prepare(page) {
            (await page.waitForElement(".new-todo")).focus();
        },
        tests: [
            new BenchmarkTestStep(`Adding${getNumberOfItemsToAdd()}Items`, (page) => {
                const numberOfItemsToAdd = getNumberOfItemsToAdd();
                const newTodo = page.querySelector(".new-todo");
                for (let i = 0; i < numberOfItemsToAdd; i++) {
                    newTodo.setValue(getTodoText(defaultLanguage, i));
                    newTodo.dispatchEvent("input");
                    newTodo.enter("keydown");
                }
            }),
            new BenchmarkTestStep("CompletingAllItems", (page) => {
                const numberOfItemsToAdd = getNumberOfItemsToAdd();
                const checkboxes = page.querySelectorAll(".toggle");
                for (let i = 0; i < numberOfItemsToAdd; i++)
                    checkboxes[i].click();
            }),
            new BenchmarkTestStep("DeletingAllItems", (page) => {
                const numberOfItemsToAdd = getNumberOfItemsToAdd();
                const deleteButtons = page.querySelectorAll(".destroy");
                for (let i = numberOfItemsToAdd - 1; i >= 0; i--)
                    deleteButtons[i].click();
            }),
        ],
    };
}
