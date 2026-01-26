/**
 * Simple work-stealing worker pool for browser environments.
 *
 * Features:
 * - Automatic worker count based on CPU cores
 * - Work-stealing: idle workers pick up tasks from the shared queue
 * - Memory-efficient: uses Transferable objects where possible
 * - Graceful error handling and worker restart
 */

/**
 * Creates a worker pool
 * @param {string} workerUrl - URL to the worker script
 * @param {Object} options
 * @param {number} options.maxWorkers - Maximum number of workers (default: navigator.hardwareConcurrency)
 * @param {number} options.taskTimeout - Timeout for individual tasks in ms (default: 60000)
 * @returns {Object} Pool instance
 */
export function createWorkerPool(workerUrl, options = {}) {
  const maxWorkers = options.maxWorkers || navigator.hardwareConcurrency || 4;
  const taskTimeout = options.taskTimeout || 60000;

  const workers = [];
  const taskQueue = [];
  const activeWorkers = new Map(); // workerId -> { task, resolve, reject, timeoutId }
  let nextWorkerId = 0;
  let isTerminated = false;

  /**
   * Creates a new worker
   */
  function createWorker() {
    const workerId = nextWorkerId++;
    const worker = new Worker(workerUrl);

    worker._id = workerId;
    worker._busy = false;

    worker.onmessage = (event) => {
      handleWorkerMessage(workerId, event.data);
    };

    worker.onerror = (error) => {
      console.error(`Worker ${workerId} error:`, error);
      handleWorkerError(workerId, error);
    };

    workers.push(worker);
    return worker;
  }

  /**
   * Handles a message from a worker
   */
  function handleWorkerMessage(workerId, data) {
    const active = activeWorkers.get(workerId);
    if (!active) return;

    clearTimeout(active.timeoutId);
    activeWorkers.delete(workerId);

    const worker = workers.find(w => w._id === workerId);
    if (worker) {
      worker._busy = false;
    }

    if (data.error) {
      active.reject(new Error(data.error));
    } else {
      active.resolve(data.result);
    }

    // Check for more work
    processQueue();
  }

  /**
   * Handles a worker error
   */
  function handleWorkerError(workerId, error) {
    const active = activeWorkers.get(workerId);
    if (active) {
      clearTimeout(active.timeoutId);
      activeWorkers.delete(workerId);
      active.reject(error);
    }

    // Remove and replace the failed worker
    const workerIndex = workers.findIndex(w => w._id === workerId);
    if (workerIndex !== -1) {
      workers[workerIndex].terminate();
      workers.splice(workerIndex, 1);
    }

    // Create a replacement worker if pool is still active
    if (!isTerminated && workers.length < maxWorkers) {
      createWorker();
    }

    processQueue();
  }

  /**
   * Finds an idle worker
   */
  function getIdleWorker() {
    return workers.find(w => !w._busy);
  }

  /**
   * Processes the task queue - assigns tasks to idle workers
   */
  function processQueue() {
    if (isTerminated) return;

    while (taskQueue.length > 0) {
      const worker = getIdleWorker();
      if (!worker) {
        // No idle workers, create more if possible
        if (workers.length < maxWorkers) {
          createWorker();
          continue;
        }
        break;
      }

      const task = taskQueue.shift();
      assignTaskToWorker(worker, task);
    }
  }

  /**
   * Assigns a task to a worker
   */
  function assignTaskToWorker(worker, task) {
    worker._busy = true;

    const timeoutId = setTimeout(() => {
      console.error(`Worker ${worker._id} task timeout`);
      handleWorkerError(worker._id, new Error("Task timeout"));
    }, taskTimeout);

    activeWorkers.set(worker._id, {
      task,
      resolve: task.resolve,
      reject: task.reject,
      timeoutId,
    });

    // Send task to worker, transferring ArrayBuffers if specified
    const message = { taskId: task.id, type: task.type, data: task.data };
    worker.postMessage(message, task.transferList || []);
  }

  /**
   * Queues a task for execution
   * @param {string} type - Task type identifier
   * @param {Object} data - Task data
   * @param {Array} transferList - Optional list of Transferable objects
   * @returns {Promise} Resolves with task result
   */
  function exec(type, data, transferList = []) {
    return new Promise((resolve, reject) => {
      if (isTerminated) {
        reject(new Error("Pool is terminated"));
        return;
      }

      const task = {
        id: `task_${Date.now()}_${Math.random().toString(36).slice(2)}`,
        type,
        data,
        transferList,
        resolve,
        reject,
      };

      taskQueue.push(task);
      processQueue();
    });
  }

  /**
   * Executes multiple tasks and returns results in order
   * @param {Array} tasks - Array of { type, data, transferList }
   * @param {Function} onProgress - Optional progress callback (completed, total)
   * @returns {Promise<Array>} Results in same order as tasks
   */
  async function execBatch(tasks, onProgress) {
    let completed = 0;
    const total = tasks.length;

    const promises = tasks.map(async (task, index) => {
      const result = await exec(task.type, task.data, task.transferList);
      completed++;
      if (onProgress) {
        onProgress(completed, total);
      }
      return { index, result };
    });

    const results = await Promise.all(promises);

    // Sort by original index to maintain order
    results.sort((a, b) => a.index - b.index);
    return results.map(r => r.result);
  }

  /**
   * Terminates all workers and cleans up
   */
  function terminate() {
    isTerminated = true;

    // Clear pending tasks
    for (const task of taskQueue) {
      task.reject(new Error("Pool terminated"));
    }
    taskQueue.length = 0;

    // Clear active tasks
    for (const [workerId, active] of activeWorkers) {
      clearTimeout(active.timeoutId);
      active.reject(new Error("Pool terminated"));
    }
    activeWorkers.clear();

    // Terminate workers
    for (const worker of workers) {
      worker.terminate();
    }
    workers.length = 0;
  }

  /**
   * Gets pool statistics
   */
  function stats() {
    return {
      workers: workers.length,
      maxWorkers,
      busyWorkers: workers.filter(w => w._busy).length,
      queuedTasks: taskQueue.length,
      isTerminated,
    };
  }

  // Initialize with one worker; more will be created on demand
  createWorker();

  return {
    exec,
    execBatch,
    terminate,
    stats,
  };
}
