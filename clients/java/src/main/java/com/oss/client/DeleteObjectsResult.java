package com.oss.client;

import java.util.ArrayList;
import java.util.Collections;
import java.util.List;

/** DeleteObjects（批量删除）结果。 */
public final class DeleteObjectsResult {

    /** 成功删除的 Key（统一契约中的 deleted）。 */
    public final List<String> deleted;
    public final List<DeleteError> errors;

    public DeleteObjectsResult(List<String> deleted, List<DeleteError> errors) {
        this.deleted = Collections.unmodifiableList(new ArrayList<>(deleted));
        this.errors = Collections.unmodifiableList(new ArrayList<>(errors));
    }

    public boolean hasErrors() {
        return !errors.isEmpty();
    }

    /** 批量删除中单个 Key 的失败信息。 */
    public static final class DeleteError {
        public final String key;
        public final String code;
        public final String message;

        public DeleteError(String key, String code, String message) {
            this.key = key;
            this.code = code;
            this.message = message;
        }

        @Override
        public String toString() {
            return "DeleteError{" + key + ", " + code + ", " + message + '}';
        }
    }

    @Override
    public String toString() {
        return "DeleteObjectsResult{deleted=" + deleted.size() + ", errors=" + errors.size() + '}';
    }
}
