<template>
  <v-row justify-md="center" justify-sm="start">
    <v-col cols="5" align-self="center">
      <span>{{ label }}</span>
    </v-col>
    <v-col cols="4" align-self="center" class="text-right">
      <span>{{ status }}</span>
    </v-col>
    <v-col cols="3" align-self="center" class="pa-0">
      <v-switch :model-value="modelValue" density="compact"
                @update:model-value="$emit('update:modelValue', $event)"
                class="float-right"
                :disabled="disabled"
      >
      </v-switch>
    </v-col>
  </v-row>
</template>

<script lang="ts">
import {defineComponent} from 'vue'

export default defineComponent({
  name: 'SettingsSwitch',
  props: {
    label: {
      type: String,
    },
    modelValue: {
      type: Boolean,
    },
    status: {
      type: String,
    },
    disabled: {
      type: Boolean,
      default: false,
    },
  },
})
</script>
