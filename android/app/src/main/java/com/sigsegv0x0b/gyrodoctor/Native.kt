package com.sigsegv0x0b.gyrodoctor

interface ProgressCallback {
    fun onProgress(stage: String, message: String, percent: Double)
}

object Native {
    init {
        System.loadLibrary("gyro_doctor")
    }

    external fun runMerge(
        inputs: Array<String>,
        output: String,
        optionsJson: String,
        callback: ProgressCallback,
    ): String

    external fun runMergeFds(
        inputFds: IntArray,
        outputFd: Int,
        optionsJson: String,
        callback: ProgressCallback,
    ): String
}
