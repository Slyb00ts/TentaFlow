// =============================================================================
// Plik: forge_coreml_shim.m
// Opis: Cienki shim C nad CoreML — bliźniak forge_metal_shim.m dla Apple
//       Neural Engine. Rust ładuje skompilowany .mlmodelc, pyta o kształty
//       wejścia/wyjścia i wywołuje predict na WŁASNYCH buforach: MLMultiArray
//       powstaje nad wskaźnikiem hosta (initWithDataPointer), a wynik trafia
//       do bufora wskazanego przez outputBackings. Kopiowania nie ma, chyba że
//       CoreML sam odrzuci podstawiony bufor — wtedy shim kopiuje i sygnalizuje
//       to kodem 1, żeby Rust mógł zalogować degradację, a nie zgadywać.
//
//       Każda funkcja eksportowana ma własną pulę autorelease: wątek roboczy
//       ANE po stronie Rusta nie ma runloopu, więc bez niej obiekty tymczasowe
//       z każdego predict żyłyby do końca procesu. Wyjątki Objective-C są
//       łapane i oddawane jako błąd — panika w cudzym wątku nie ma prawa
//       zabić procesu.
//
//       Konwencja zwrotów: 0 = sukces w miejscu, 1 = sukces z kopią,
//       wartości ujemne = błąd (opis w buforze err, gdy funkcja go przyjmuje).
// =============================================================================

#import <CoreML/CoreML.h>
#import <Foundation/Foundation.h>

#include <string.h>

/// Kody błędów wspólne dla wszystkich funkcji shimu.
enum {
    FC_OK = 0,
    FC_COPIED = 1,
    FC_ERR_ARGS = -1,
    FC_ERR_NO_INPUT = -2,
    FC_ERR_INPUT_SHAPE = -3,
    FC_ERR_NO_OUTPUT = -4,
    FC_ERR_OUTPUT_SHAPE = -5,
    FC_ERR_MULTIARRAY = -6,
    FC_ERR_PROVIDER = -7,
    FC_ERR_PREDICT = -8,
    FC_ERR_RESULT = -9,
    FC_ERR_RESULT_TYPE = -10,
    FC_ERR_DTYPE = -11,
    FC_ERR_EXCEPTION = -12,
};

static void fc_copy_error(NSError *err, char *out, int out_len) {
    if (out == NULL || out_len <= 0) {
        return;
    }
    const char *msg = err ? [[err localizedDescription] UTF8String] : "unknown CoreML error";
    snprintf(out, (size_t)out_len, "%s", msg ? msg : "unknown CoreML error");
}

static void fc_set_error(const char *msg, char *out, int out_len) {
    if (out == NULL || out_len <= 0) {
        return;
    }
    snprintf(out, (size_t)out_len, "%s", msg);
}

static void fc_set_exception(NSException *e, char *out, int out_len) {
    if (out == NULL || out_len <= 0) {
        return;
    }
    const char *name = e.name ? [e.name UTF8String] : "NSException";
    const char *reason = e.reason ? [e.reason UTF8String] : "bez powodu";
    snprintf(out, (size_t)out_len, "wyjątek Objective-C %s: %s", name ? name : "NSException",
             reason ? reason : "bez powodu");
}

int fc_available(void) {
    return 1;
}

void *fc_model_load(const char *mlmodelc_path,
                    const char *function_name,
                    int compute_units,
                    char *err,
                    int err_len) {
    @autoreleasepool {
        if (mlmodelc_path == NULL) {
            fc_set_error("brak ścieżki do modelu", err, err_len);
            return NULL;
        }
        @try {
            MLModelConfiguration *cfg = [[MLModelConfiguration alloc] init];
            switch (compute_units) {
                case 1:
                    cfg.computeUnits = MLComputeUnitsAll;
                    break;
                case 2:
                    cfg.computeUnits = MLComputeUnitsCPUOnly;
                    break;
                case 3:
                    cfg.computeUnits = MLComputeUnitsCPUAndGPU;
                    break;
                default:
                    cfg.computeUnits = MLComputeUnitsCPUAndNeuralEngine;
                    break;
            }
            if (function_name != NULL && function_name[0] != '\0') {
                // Modele wielofunkcyjne (mlprogram z kilkoma wejściami `main`)
                // istnieją od macOS 15; na starszym systemie nazwa funkcji jest
                // po prostu ignorowana, a model ładuje się z funkcją domyślną.
                if (@available(macOS 15.0, iOS 18.0, *)) {
                    cfg.functionName = [NSString stringWithUTF8String:function_name];
                }
            }
            NSURL *url =
                [NSURL fileURLWithPath:[NSString stringWithUTF8String:mlmodelc_path]];
            NSError *error = nil;
            MLModel *model = [MLModel modelWithContentsOfURL:url configuration:cfg error:&error];
            if (model == nil) {
                fc_copy_error(error, err, err_len);
                return NULL;
            }
            return (__bridge_retained void *)model;
        } @catch (NSException *e) {
            fc_set_exception(e, err, err_len);
            return NULL;
        }
    }
}

/// Kształt 2D [rows, cols] z ograniczenia MLMultiArray w opisie modelu.
/// Zwraca 0, gdy cecha istnieje, jest MultiArray f16 i ma dokładnie 2 wymiary.
/// Typ jest sprawdzany TU, bo predict zakłada f16 po obu stronach — model
/// f32 nie zgłosiłby błędu, tylko czytał połowę wejścia i pisał podwójnie.
static int fc_shape_2d(NSDictionary<NSString *, MLFeatureDescription *> *descs,
                       const char *name,
                       long long *rows,
                       long long *cols,
                       int err_missing,
                       int err_shape) {
    if (name == NULL) {
        return FC_ERR_ARGS;
    }
    MLFeatureDescription *desc = descs[[NSString stringWithUTF8String:name]];
    if (desc == nil) {
        return err_missing;
    }
    MLMultiArrayConstraint *c = desc.multiArrayConstraint;
    if (c == nil || c.shape.count != 2) {
        return err_shape;
    }
    if (c.dataType != MLMultiArrayDataTypeFloat16) {
        return FC_ERR_DTYPE;
    }
    if (rows) {
        *rows = c.shape[0].longLongValue;
    }
    if (cols) {
        *cols = c.shape[1].longLongValue;
    }
    return FC_OK;
}

int fc_model_shape(void *model,
                   const char *in_name,
                   long long *in_rows,
                   long long *in_cols,
                   const char *out_name,
                   long long *out_rows,
                   long long *out_cols) {
    @autoreleasepool {
        if (model == NULL) {
            return FC_ERR_ARGS;
        }
        MLModel *m = (__bridge MLModel *)model;
        MLModelDescription *d = m.modelDescription;
        int rc = fc_shape_2d(d.inputDescriptionsByName, in_name, in_rows, in_cols,
                             FC_ERR_NO_INPUT, FC_ERR_INPUT_SHAPE);
        if (rc != FC_OK) {
            return rc;
        }
        return fc_shape_2d(d.outputDescriptionsByName, out_name, out_rows, out_cols,
                           FC_ERR_NO_OUTPUT, FC_ERR_OUTPUT_SHAPE);
    }
}

/// Kopia wyniku [out_rows, out_cols] f16 ze wskaźnika `src` (z krokami
/// `got`) do `dst` o kroku wiersza `out_row_stride`.
static void fc_copy_rows(const uint16_t *src,
                         MLMultiArray *got,
                         uint16_t *dst,
                         long long out_rows,
                         long long out_cols,
                         long long out_row_stride) {
    long long src_row_stride = got.strides[0].longLongValue;
    long long src_col_stride = got.strides[1].longLongValue;
    for (long long r = 0; r < out_rows; ++r) {
        const uint16_t *src_row = src + r * src_row_stride;
        uint16_t *dst_row = dst + r * out_row_stride;
        if (src_col_stride == 1) {
            memcpy(dst_row, src_row, (size_t)out_cols * sizeof(uint16_t));
        } else {
            for (long long c = 0; c < out_cols; ++c) {
                dst_row[c] = src_row[c * src_col_stride];
            }
        }
    }
}

/// Wspólna ścieżka predict. `out_row_stride` w elementach; 0 oznacza układ
/// ciągły ([rows, cols] bez wypełnienia).
static int fc_predict_impl(void *model,
                           const char *in_name,
                           const void *in_ptr,
                           const char *out_name,
                           void *out_ptr,
                           long long out_row_stride,
                           char *err,
                           int err_len) {
    if (model == NULL || in_name == NULL || in_ptr == NULL || out_name == NULL || out_ptr == NULL) {
        fc_set_error("argument NULL w fc_model_predict", err, err_len);
        return FC_ERR_ARGS;
    }
    @try {
        MLModel *m = (__bridge MLModel *)model;
        MLModelDescription *d = m.modelDescription;

        long long in_rows = 0, in_cols = 0, out_rows = 0, out_cols = 0;
        int rc = fc_shape_2d(d.inputDescriptionsByName, in_name, &in_rows, &in_cols,
                             FC_ERR_NO_INPUT, FC_ERR_INPUT_SHAPE);
        if (rc != FC_OK) {
            fc_set_error("wejście modelu nie istnieje lub nie jest MultiArray 2D f16", err,
                         err_len);
            return rc;
        }
        rc = fc_shape_2d(d.outputDescriptionsByName, out_name, &out_rows, &out_cols,
                         FC_ERR_NO_OUTPUT, FC_ERR_OUTPUT_SHAPE);
        if (rc != FC_OK) {
            fc_set_error("wyjście modelu nie istnieje lub nie jest MultiArray 2D f16", err,
                         err_len);
            return rc;
        }
        if (out_row_stride == 0) {
            out_row_stride = out_cols;
        }
        if (out_row_stride < out_cols) {
            fc_set_error("krok wiersza wyjścia mniejszy niż liczba kolumn", err, err_len);
            return FC_ERR_ARGS;
        }

        NSString *in_key = [NSString stringWithUTF8String:in_name];
        NSString *out_key = [NSString stringWithUTF8String:out_name];
        NSError *error = nil;

        // Wejście: widok nad pamięcią wołającego. Deallocator nil — Rust jest
        // właścicielem i żyje dłużej niż to wywołanie.
        MLMultiArray *x = [[MLMultiArray alloc] initWithDataPointer:(void *)in_ptr
                                                              shape:@[ @(in_rows), @(in_cols) ]
                                                           dataType:MLMultiArrayDataTypeFloat16
                                                            strides:@[ @(in_cols), @(1) ]
                                                        deallocator:nil
                                                              error:&error];
        if (x == nil) {
            fc_copy_error(error, err, err_len);
            return FC_ERR_MULTIARRAY;
        }
        MLMultiArray *y = [[MLMultiArray alloc] initWithDataPointer:out_ptr
                                                              shape:@[ @(out_rows), @(out_cols) ]
                                                           dataType:MLMultiArrayDataTypeFloat16
                                                            strides:@[ @(out_row_stride), @(1) ]
                                                        deallocator:nil
                                                              error:&error];
        if (y == nil) {
            fc_copy_error(error, err, err_len);
            return FC_ERR_MULTIARRAY;
        }

        MLDictionaryFeatureProvider *in = [[MLDictionaryFeatureProvider alloc]
            initWithDictionary:@{ in_key : [MLFeatureValue featureValueWithMultiArray:x] }
                         error:&error];
        if (in == nil) {
            fc_copy_error(error, err, err_len);
            return FC_ERR_PROVIDER;
        }
        MLPredictionOptions *opts = [[MLPredictionOptions alloc] init];
        opts.outputBackings = @{ out_key : y };

        id<MLFeatureProvider> result = [m predictionFromFeatures:in options:opts error:&error];
        if (result == nil) {
            fc_copy_error(error, err, err_len);
            return FC_ERR_PREDICT;
        }

        MLFeatureValue *fv = [result featureValueForName:out_key];
        MLMultiArray *got = fv.multiArrayValue;
        if (got == nil) {
            fc_set_error("wynik predict nie zawiera wyjścia MultiArray", err, err_len);
            return FC_ERR_RESULT;
        }

        // Ścieżka szczęśliwa: CoreML oddał DOKŁADNIE nasz obiekt backing,
        // więc wynik leży w out_ptr. Tożsamość obiektu jest tańsza i pewniejsza
        // niż porównanie adresów.
        if (got == y) {
            return FC_OK;
        }

        // CoreML nie oddał naszego obiektu. Zanim skopiujemy, sprawdzamy typ
        // i kształt — inny kształt to błąd, nie kopia.
        if (got.dataType != MLMultiArrayDataTypeFloat16 || got.shape.count != 2 ||
            got.shape[0].longLongValue != out_rows || got.shape[1].longLongValue != out_cols) {
            fc_set_error("wynik predict ma inny typ lub kształt niż deklaracja modelu", err,
                         err_len);
            return FC_ERR_RESULT_TYPE;
        }

        // Kopia przez getBytesWithHandler: (macOS 12.3+) — jedyne wspierane
        // wejście do pamięci cudzej tablicy. Jeśli adres okaże się nasz (inny
        // obiekt nad tym samym buforem), nic nie kopiujemy.
        __block int copy_rc = FC_OK;
        if (@available(macOS 12.3, iOS 15.4, *)) {
            [got getBytesWithHandler:^(const void *bytes, NSInteger size) {
                (void)size;
                if (bytes == out_ptr) {
                    copy_rc = FC_OK;
                    return;
                }
                fc_copy_rows((const uint16_t *)bytes, got, (uint16_t *)out_ptr, out_rows,
                             out_cols, out_row_stride);
                copy_rc = FC_COPIED;
            }];
            return copy_rc;
        }

        // Starszy system: `dataPointer` jest przestarzały od macOS 13, ale to
        // jedyna droga do pamięci wyniku.
#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        const void *got_ptr = got.dataPointer;
#pragma clang diagnostic pop
        if (got_ptr == out_ptr) {
            return FC_OK;
        }
        fc_copy_rows((const uint16_t *)got_ptr, got, (uint16_t *)out_ptr, out_rows, out_cols,
                     out_row_stride);
        return FC_COPIED;
    } @catch (NSException *e) {
        fc_set_exception(e, err, err_len);
        return FC_ERR_EXCEPTION;
    }
}

int fc_model_predict(void *model,
                     const char *in_name,
                     const void *in_ptr,
                     const char *out_name,
                     void *out_ptr,
                     char *err,
                     int err_len) {
    @autoreleasepool {
        return fc_predict_impl(model, in_name, in_ptr, out_name, out_ptr, 0, err, err_len);
    }
}

int fc_model_predict_strided(void *model,
                             const char *in_name,
                             const void *in_ptr,
                             const char *out_name,
                             void *out_ptr,
                             long long out_row_stride_elems,
                             char *err,
                             int err_len) {
    @autoreleasepool {
        if (out_row_stride_elems <= 0) {
            fc_set_error("krok wiersza wyjścia musi być dodatni", err, err_len);
            return FC_ERR_ARGS;
        }
        return fc_predict_impl(model, in_name, in_ptr, out_name, out_ptr, out_row_stride_elems,
                               err, err_len);
    }
}

void fc_model_release(void *model) {
    @autoreleasepool {
        if (model == NULL) {
            return;
        }
        MLModel *m = (__bridge_transfer MLModel *)model;
        (void)m;
    }
}
